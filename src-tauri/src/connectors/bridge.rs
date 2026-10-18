//! The chat-platform half of a connector: one conversation on a platform
//! (a Discord channel, thread or DM) is one OpenLeash chat.
//!
//! The bridge is platform-agnostic on purpose. A platform adapter
//! (`discord.rs`) owns the network and the *gate* (who may talk to the agent at
//! all); it hands the bridge messages that already passed, and drains the
//! [`Outgoing`] queue the bridge fills. Everything about chats lives here:
//!
//! - the first message in a conversation creates a chat; later ones continue it
//!   (and steer a turn that is still running, exactly like typing in the app);
//! - a *watcher* follows each turn and posts the agent's text back as it is
//!   finished, relays approvals and `ask_user` questions, and keeps the
//!   platform's typing indicator alive while the agent works;
//! - the next message after a relayed approval or question is the answer.
//!
//! Nothing here widens what an agent may do: a connector chat runs under the
//! permission mode the user picked for it, and an approval is answered through
//! the same pending channel the app's own buttons use.

use crate::agent::{runner, Harness, Item};
use chrono::{DateTime, Utc};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, Mutex};

/// How often a watcher looks at its turn. Polling the task rather than
/// listening to the event bus keeps the bridge free of the app handle (so it is
/// testable against a bare harness), and a second is well under how fast anyone
/// reads a chat message.
const POLL: Duration = Duration::from_millis(900);
/// A turn that never starts running (a send that failed inside the runner, a
/// paused account) is given up on after this long.
const STARTUP_GRACE: Duration = Duration::from_secs(15);
/// Discord's typing indicator lasts ten seconds.
const TYPING_EVERY: Duration = Duration::from_secs(8);
/// Inbound text is capped: it is somebody's chat message, not a file transfer.
const MAX_INBOUND: usize = 8_000;

/// One message from a person on a platform, already through its gate.
#[derive(Debug, Clone)]
pub struct Incoming {
    /// The platform's conversation id (Discord: the channel, thread or DM id).
    pub conv: String,
    pub message_id: String,
    /// A display handle, already sanitised by the adapter.
    pub author: String,
    pub author_id: String,
    /// Where it was said, for the agent: "a direct message", "#general in Acme".
    pub place: String,
    pub text: String,
    /// `name — url` lines for files the person attached.
    pub attachments: Vec<String>,
}

/// What the adapter must do on the platform.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// Post a message (the adapter splits it to the platform's size limit).
    Post {
        conv: String,
        text: String,
        reply_to: Option<String>,
    },
    /// Show "typing…" in the conversation.
    Typing { conv: String },
}

/// How new chats from this platform are created. Read fresh for every new chat,
/// so a settings change applies to the next conversation without a restart.
#[derive(Debug, Clone)]
pub struct ChatDefaults {
    pub platform: &'static str,
    pub project: String,
    pub model: String,
    pub perm: String,
}

/// The approval or question the bridge relayed and is waiting on.
#[derive(Debug, Clone)]
struct Awaiting {
    task_id: String,
    item_id: String,
    kind: String,
    questions: Vec<Value>,
}

#[derive(Default)]
struct Conv {
    /// A watcher is following this conversation's chat.
    watching: bool,
    /// Bumped by every message handed to the chat, so a watcher that decided
    /// its turn was over can tell a new one began in the same instant.
    gen: u64,
    /// The latest inbound message, which the next reply answers.
    reply_to: Option<String>,
    awaiting: Option<Awaiting>,
}

pub struct Bridge {
    h: Arc<Harness>,
    out: mpsc::UnboundedSender<Outgoing>,
    /// Conversation id → chat id, persisted so a conversation continues the
    /// same chat across restarts.
    links: Mutex<HashMap<String, String>>,
    links_path: PathBuf,
    convs: Mutex<HashMap<String, Conv>>,
    /// One message at a time per conversation: two quick messages must not
    /// both decide there is no chat yet and create two.
    turns: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

/// A slash command the bridge answers itself, rather than the agent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Cmd {
    New,
    Stop,
    Status,
    Help,
}

/// `/new`, `!new` and friends. Only an exact command word counts: `/newline`
/// or "!new idea" is an ordinary message.
pub fn command(text: &str) -> Option<Cmd> {
    let t = text.trim();
    let word = t.strip_prefix('/').or_else(|| t.strip_prefix('!'))?;
    match word.to_ascii_lowercase().as_str() {
        "new" | "reset" => Some(Cmd::New),
        "stop" | "cancel" => Some(Cmd::Stop),
        "status" => Some(Cmd::Status),
        "help" => Some(Cmd::Help),
        _ => None,
    }
}

const HELP: &str = "I'm your OpenLeash agent. Message me and I'll work on it in a chat on your computer. Every channel, thread or DM is its own chat, and it carries on until you start a fresh one.\n\
`/new`: start a fresh chat here\n\
`/stop`: stop what I'm doing\n\
`/status`: which chat this is and what it's doing\n\
`/compact`, `/goal …`, `/btw …`: same as in the app\n\
When I need your approval or ask a question, just reply.";

/// The command mark people can actually type on a platform. Slack takes
/// every message that starts with `/` as one of its own slash commands and
/// never delivers it to the bot, so there the `!` forms are the only ones
/// that work, and telling people to type `/new` would be a dead end.
fn mark(platform: &str) -> char {
    if platform == "Slack" {
        '!'
    } else {
        '/'
    }
}

/// [`HELP`] with the commands written the way this platform lets you send them.
pub fn help(platform: &str) -> String {
    match mark(platform) {
        '/' => HELP.to_string(),
        m => HELP.replace("`/", &format!("`{m}")),
    }
}

/// The commands the app's backend owns (`/compact`, `/goal …`, `/btw …`),
/// normalised to the `/` form its dispatcher expects. `!compact` counts too,
/// for platforms where `/` can't be sent; only those exact words do, so
/// "!important: …" is still an ordinary message for the agent.
pub fn backend_command(text: &str) -> Option<String> {
    if text.starts_with('/') {
        return Some(text.to_string());
    }
    let rest = text.strip_prefix('!')?;
    let word = rest.split_whitespace().next()?.to_ascii_lowercase();
    let exact = rest
        .get(word.len()..)
        .is_some_and(|after| after.is_empty() || after.starts_with(char::is_whitespace));
    (exact && matches!(word.as_str(), "compact" | "goal" | "btw")).then(|| format!("/{rest}"))
}

/// The reply to a relayed approval, as the approval card would have sent it.
/// Anything that is not a clear yes is a *deny*. The text goes along as
/// feedback, so "no, run the tests first" still reaches the agent.
pub fn approval_reply(text: &str) -> Value {
    let t = text.trim();
    let first = t
        .split(|c: char| c.is_whitespace() || c == ',' || c == '.' || c == '!')
        .next()
        .unwrap_or("")
        .to_lowercase();
    let whole = t.to_lowercase();
    if matches!(whole.as_str(), "yes always" | "always allow" | "always") {
        return json!({"decision": "always"});
    }
    if t.chars().count() <= 24
        && matches!(
            first.as_str(),
            "yes"
                | "y"
                | "yep"
                | "yeah"
                | "ok"
                | "okay"
                | "approve"
                | "approved"
                | "allow"
                | "once"
                | "go"
                | "sure"
                | "👍"
                | "✅"
        )
    {
        return json!({"decision": "once"});
    }
    if matches!(
        first.as_str(),
        "no" | "n" | "nope" | "deny" | "denied" | "reject" | "stop" | "👎" | "❌"
    ) && t.chars().count() <= 24
    {
        return json!({"decision": "deny"});
    }
    json!({"decision": "deny", "feedback": t})
}

fn option_label(q: &Value, n: usize) -> Option<String> {
    let opts = q["options"].as_array()?;
    let o = opts.get(n.checked_sub(1)?)?;
    o["label"]
        .as_str()
        .map(String::from)
        .or_else(|| o.as_str().map(String::from))
}

fn one_answer(q: &Value, raw: &str) -> Value {
    let raw = raw.trim();
    if raw.is_empty() || matches!(raw.to_lowercase().as_str(), "skip" | "-") {
        return Value::Null;
    }
    let pick = |s: &str| {
        s.trim()
            .parse::<usize>()
            .ok()
            .and_then(|n| option_label(q, n))
            .unwrap_or_else(|| s.trim().to_string())
    };
    match q["type"].as_str().unwrap_or("text") {
        "single" => json!(pick(raw)),
        "multi" => json!(raw
            .split(',')
            .map(pick)
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()),
        "confirm" => match raw.to_lowercase().as_str() {
            "y" | "yes" | "true" | "ok" | "sure" | "yep" => json!(true),
            "n" | "no" | "false" | "nope" => json!(false),
            _ => json!(raw),
        },
        "number" => raw
            .parse::<f64>()
            .ok()
            .and_then(serde_json::Number::from_f64)
            .map(Value::Number)
            .unwrap_or(Value::Null),
        _ => json!(raw),
    }
}

/// The reply to a relayed `ask_user` form, in the shape the question card
/// sends. One question takes the whole reply; several take one line each, in
/// order. Option numbers pick that option; anything else is a written answer.
pub fn question_reply(qs: &[Value], text: &str) -> Value {
    let t = text.trim();
    if matches!(
        t.to_lowercase().as_str(),
        "skip" | "dismiss" | "you decide" | "your call" | "whatever"
    ) {
        return json!({"dismissed": true, "note": "Answered from chat: use your judgment."});
    }
    let lines: Vec<&str> = if qs.len() <= 1 {
        vec![t]
    } else {
        t.lines().filter(|l| !l.trim().is_empty()).collect()
    };
    let answers: Vec<Value> = qs
        .iter()
        .enumerate()
        .map(|(i, q)| {
            lines
                .get(i)
                .map(|l| one_answer(q, l))
                .unwrap_or(Value::Null)
        })
        .collect();
    let skipped: Vec<usize> = answers
        .iter()
        .enumerate()
        .filter(|(_, a)| a.is_null())
        .map(|(i, _)| i)
        .collect();
    json!({"answers": answers, "notes": vec![""; qs.len()], "skipped": skipped})
}

fn clip(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n).collect::<String>())
    }
}

/// An approval card, written for a chat message.
pub fn render_approval(item: &Item) -> String {
    let d = &item.data;
    let title = d["title"]
        .as_str()
        .filter(|s| !s.is_empty())
        .unwrap_or(&item.text);
    let mut s = format!("🔐 **Approval needed:** {}", clip(title, 200));
    if let Some(detail) = d["detail"].as_str().filter(|s| !s.trim().is_empty()) {
        s.push_str(&format!(
            "\n```\n{}\n```",
            clip(&detail.replace("```", "'''"), 1200)
        ));
    }
    if let Some(r) = d["reason"].as_str().filter(|s| !s.trim().is_empty()) {
        s.push_str(&format!("\n{}", clip(r, 400)));
    }
    s.push_str("\nReply **yes** to allow it once, **always** to allow it from now on, or **no** to deny. Anything else denies it and goes to the agent as feedback.");
    s
}

/// An `ask_user` form, written for a chat message.
pub fn render_question(item: &Item, qs: &[Value]) -> String {
    let title = item.data["title"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or("The agent has a question");
    let mut s = format!("❓ **{}**", clip(title, 200));
    if let Some(intro) = item.data["intro"].as_str().filter(|s| !s.trim().is_empty()) {
        s.push_str(&format!("\n{}", clip(intro, 600)));
    }
    for (i, q) in qs.iter().enumerate() {
        let text = q["question"].as_str().unwrap_or("");
        s.push_str(&if qs.len() > 1 {
            format!("\n**{}.** {}", i + 1, clip(text, 500))
        } else {
            format!("\n{}", clip(text, 500))
        });
        if q["type"].as_str() == Some("confirm") {
            s.push_str(" *(yes / no)*");
        }
        if let Some(opts) = q["options"].as_array() {
            // A long list (up to 30) at the full 160 chars a line is a wall of
            // text in chat; shorter lines keep the numbers scannable.
            let desc_max = if opts.len() > 10 { 80 } else { 160 };
            for (k, o) in opts.iter().enumerate() {
                let label = o["label"].as_str().unwrap_or("");
                let desc = o["description"].as_str().filter(|d| !d.is_empty());
                let star = if o["recommended"].as_bool() == Some(true) {
                    " ★"
                } else {
                    ""
                };
                s.push_str(&format!(
                    "\n  `{}` {}{}{}",
                    k + 1,
                    clip(label, 100),
                    star,
                    desc.map(|d| format!(" · {}", clip(d, desc_max)))
                        .unwrap_or_default()
                ));
            }
        }
    }
    s.push_str(if qs.len() > 1 {
        "\nReply with one line per question, in order. Use an option's number or write your own answer. **skip** lets the agent decide."
    } else {
        "\nReply with an option's number or your own answer. **skip** lets the agent decide."
    });
    s
}

/// Strip what only renders inside OpenLeash. The agent is told its replies go
/// to a chat platform, but a habit (or a chat that started in the app) can
/// still produce an interactive preview, which would arrive as a wall of code.
pub fn for_chat(text: &str) -> String {
    let mut out = String::new();
    let mut skipping = false;
    let mut said = false;
    for line in text.split('\n') {
        let t = line.trim_start();
        if !skipping && t.starts_with("```openleash-") {
            skipping = true;
            if !said {
                out.push_str("*(interactive preview: open this chat in OpenLeash to see it)*\n");
                said = true;
            }
            continue;
        }
        if skipping {
            if t.starts_with("```") {
                skipping = false;
            }
            continue;
        }
        out.push_str(line);
        out.push('\n');
    }
    out.trim_end().to_string()
}

/// A display string from a platform, made safe to embed in a prompt line.
pub fn sanitize(s: &str, max: usize) -> String {
    let s: String = s
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '<' | '>' | '`'))
        .collect();
    clip(s.trim(), max)
}

struct Snap {
    status: String,
    waiting_kind: Option<String>,
    running: bool,
    items: Vec<Item>,
}

impl Bridge {
    pub fn new(
        h: Arc<Harness>,
        out: mpsc::UnboundedSender<Outgoing>,
        links_path: PathBuf,
    ) -> Arc<Bridge> {
        let links: HashMap<String, String> = std::fs::read_to_string(&links_path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();
        Arc::new(Bridge {
            h,
            out,
            links: Mutex::new(links),
            links_path,
            convs: Default::default(),
            turns: Default::default(),
        })
    }

    pub async fn chat_count(&self) -> usize {
        self.links.lock().await.len()
    }

    pub async fn forget_all(&self) {
        self.links.lock().await.clear();
        self.convs.lock().await.clear();
        self.persist().await;
    }

    async fn persist(&self) {
        let links = self.links.lock().await.clone();
        if let Ok(s) = serde_json::to_string_pretty(&links) {
            let tmp = self.links_path.with_extension("json.tmp");
            if std::fs::write(&tmp, s).is_ok() {
                let _ = std::fs::rename(&tmp, &self.links_path);
            }
        }
    }

    fn post(&self, conv: &str, text: impl Into<String>, reply_to: Option<String>) {
        let _ = self.out.send(Outgoing::Post {
            conv: conv.to_string(),
            text: text.into(),
            reply_to,
        });
    }

    /// The chat this conversation continues, if it still exists. A chat the
    /// user deleted or archived in the app ends the link: the next message
    /// starts a fresh one instead of reviving something they put away.
    async fn linked(&self, conv: &str) -> Option<String> {
        let id = self.links.lock().await.get(conv).cloned()?;
        let alive = match self.h.task(&id).await {
            Ok(t) => !t.lock().await.archived,
            Err(_) => false,
        };
        if alive {
            Some(id)
        } else {
            self.links.lock().await.remove(conv);
            self.persist().await;
            None
        }
    }

    /// Handle one message. Serialised per conversation.
    pub async fn handle(self: &Arc<Self>, m: Incoming, defaults: ChatDefaults) {
        let lock = {
            let mut turns = self.turns.lock().await;
            turns.entry(m.conv.clone()).or_default().clone()
        };
        let _turn = lock.lock().await;
        let linked = self.linked(&m.conv).await;
        let reply = Some(m.message_id.clone());
        match command(&m.text) {
            Some(Cmd::New) => {
                self.links.lock().await.remove(&m.conv);
                self.persist().await;
                if let Some(c) = self.convs.lock().await.get_mut(&m.conv) {
                    c.awaiting = None;
                }
                self.post(&m.conv, "🆕 Fresh start: your next message opens a new chat. The old one is still in OpenLeash.", reply);
                return;
            }
            Some(Cmd::Stop) => {
                let msg = match &linked {
                    Some(id) if self.h.runtime(id).running.load(Ordering::SeqCst) => {
                        runner::interrupt(&self.h, id).await;
                        "⏹ Stopped."
                    }
                    _ => "Nothing is running here.",
                };
                self.post(&m.conv, msg, reply);
                return;
            }
            Some(Cmd::Status) => {
                let msg = match &linked {
                    Some(id) => match self.h.task(id).await {
                        Ok(t) => {
                            let t = t.lock().await;
                            format!(
                                "💬 **{}**\nStatus: {}{} · model `{}` · permissions `{}`\nProject: `{}`",
                                t.title,
                                t.status,
                                t.waiting_kind.as_deref().map(|k| format!(" ({k})")).unwrap_or_default(),
                                t.model,
                                t.perm,
                                t.project
                            )
                        }
                        Err(_) => "No chat yet. Send a message to start one.".into(),
                    },
                    None => "No chat yet. Send a message to start one.".into(),
                };
                self.post(&m.conv, msg, reply);
                return;
            }
            Some(Cmd::Help) => {
                self.post(&m.conv, help(defaults.platform), reply);
                return;
            }
            None => {}
        }
        if let Some(id) = &linked {
            if self.answer(&m, id).await {
                return;
            }
        }
        let text: String = m.text.trim().chars().take(MAX_INBOUND).collect();
        if text.is_empty() && m.attachments.is_empty() {
            return;
        }
        let mut display = text.clone();
        if !m.attachments.is_empty() {
            display.push_str(&format!(
                "\n\nAttached in {}:\n{}",
                defaults.platform,
                m.attachments.join("\n")
            ));
        }
        let since = Utc::now();
        let task_id = match linked {
            Some(id) => {
                crate::wait_for_task_startup(&self.h, &id).await;
                // Commands the app's backend owns (`/compact`, `/goal`, `/btw`…)
                // go through the same dispatcher as the composer.
                let r = if let Some(cmd) = backend_command(&text) {
                    crate::dispatch(self.h.clone(), id.clone(), cmd).await
                } else {
                    let framed = frame(&m, &display, defaults.platform, false);
                    runner::send_as(self.h.clone(), id.clone(), display.clone(), framed).await
                };
                if let Err(e) = r {
                    self.post(
                        &m.conv,
                        format!("⚠️ Couldn't send that to the chat: {}", clip(&e, 400)),
                        reply,
                    );
                    return;
                }
                id
            }
            None => match self.create(&m, &display, &defaults).await {
                Ok(id) => id,
                Err(e) => {
                    self.post(
                        &m.conv,
                        format!("⚠️ Couldn't start a chat: {}", clip(&e, 600)),
                        reply,
                    );
                    return;
                }
            },
        };
        self.watch(&m.conv, &task_id, since, m.message_id.clone())
            .await;
    }

    async fn create(
        &self,
        m: &Incoming,
        display: &str,
        d: &ChatDefaults,
    ) -> Result<String, String> {
        let s = self.h.settings.read().await.clone();
        let project = if d.project.trim().is_empty() {
            s.project.clone()
        } else {
            d.project.clone()
        };
        if project.trim().is_empty() {
            return Err(format!("no project folder is set for {} chats. Pick one in OpenLeash → Settings → Connectors.", d.platform));
        }
        let model = if !d.model.trim().is_empty() {
            d.model.clone()
        } else {
            s.recent_models
                .first()
                .cloned()
                .unwrap_or_else(|| s.model.clone())
        };
        if model.trim().is_empty() {
            return Err(
                "no model is set. Pick one for this connector in Settings → Connectors.".into(),
            );
        }
        let req = crate::NewTask {
            prompt: frame(m, display, d.platform, true),
            project,
            model,
            effort: s.effort,
            perm: d.perm.clone(),
            plan: false,
            worktree: false,
            workspace: Some("local".into()),
            base_branch: String::new(),
            assist: s.assist.clone(),
            agents: (!s.default_agents.is_empty()).then(|| s.default_agents.clone()),
            route: None,
            images: vec![],
            files: vec![],
            image_names: vec![],
            ultra: false,
            ultra_wt: false,
            ultra_x: None,
        };
        let summary = crate::create_task(self.h.clone(), req, Some(display.to_string())).await?;
        // The app learns about a chat it did not create from this event; the
        // composer's own create call gets the summary back instead.
        if let Ok(t) = self.h.task(&summary.id).await {
            self.h.emit_summary(&*t.lock().await);
        }
        self.links
            .lock()
            .await
            .insert(m.conv.clone(), summary.id.clone());
        self.persist().await;
        Ok(summary.id)
    }

    /// If this conversation owes an answer to a relayed approval or question,
    /// this message is it. Returns whether the message was used up.
    async fn answer(&self, m: &Incoming, task_id: &str) -> bool {
        let aw = {
            let mut convs = self.convs.lock().await;
            let Some(c) = convs.get_mut(&m.conv) else {
                return false;
            };
            match &c.awaiting {
                Some(a) if a.task_id == task_id => c.awaiting.take(),
                _ => None,
            }
        };
        let Some(aw) = aw else { return false };
        let tx = self
            .h
            .runtime(task_id)
            .pending
            .lock()
            .await
            .remove(&aw.item_id);
        // Answered in the app meanwhile, or the run was stopped: the message
        // is an ordinary one after all.
        let Some(tx) = tx else { return false };
        let resp = if aw.kind == "approval" {
            approval_reply(&m.text)
        } else {
            question_reply(&aw.questions, &m.text)
        };
        self.h.touch(task_id).await;
        let _ = tx.send(resp);
        let mut convs = self.convs.lock().await;
        let c = convs.entry(m.conv.clone()).or_default();
        c.reply_to = Some(m.message_id.clone());
        c.gen += 1;
        true
    }

    /// Make sure a watcher is following this conversation's turn.
    async fn watch(
        self: &Arc<Self>,
        conv: &str,
        task_id: &str,
        since: DateTime<Utc>,
        reply_to: String,
    ) {
        {
            let mut convs = self.convs.lock().await;
            let c = convs.entry(conv.to_string()).or_default();
            c.gen += 1;
            c.reply_to = Some(reply_to);
            if c.watching {
                return;
            }
            c.watching = true;
        }
        let me = self.clone();
        let (conv, task_id) = (conv.to_string(), task_id.to_string());
        tokio::spawn(async move { me.follow(conv, task_id, since).await });
    }

    async fn snapshot(&self, task_id: &str, since: DateTime<Utc>) -> Option<Snap> {
        let t = self.h.task(task_id).await.ok()?;
        let running = self.h.runtime(task_id).running.load(Ordering::SeqCst);
        let g = t.lock().await;
        Some(Snap {
            status: g.status.clone(),
            waiting_kind: g.waiting_kind.clone(),
            running,
            items: g.items.iter().filter(|i| i.ts >= since).cloned().collect(),
        })
    }

    fn reply_target(&self, convs: &HashMap<String, Conv>, conv: &str) -> Option<String> {
        convs.get(conv).and_then(|c| c.reply_to.clone())
    }

    /// Follow one conversation's chat until its turn is over, posting as it goes.
    async fn follow(self: Arc<Self>, conv: String, task_id: String, since: DateTime<Utc>) {
        let mut posted: HashSet<String> = HashSet::new();
        let mut last_text: HashMap<String, String> = HashMap::new();
        let mut seen_running = false;
        let mut started = Instant::now();
        let mut over_before = false;
        let mut said = false;
        let _ = self.out.send(Outgoing::Typing { conv: conv.clone() });
        let mut last_typing = Instant::now();
        loop {
            tokio::time::sleep(POLL).await;
            let gen = self
                .convs
                .lock()
                .await
                .get(&conv)
                .map(|c| c.gen)
                .unwrap_or(0);
            let Some(snap) = self.snapshot(&task_id, since).await else {
                break;
            };
            seen_running |= snap.running || snap.status == "running";
            let asking = snap.status == "waiting"
                && matches!(snap.waiting_kind.as_deref(), Some("approval" | "question"));
            let over = !snap.running
                && snap.status != "running"
                && !asking
                && (seen_running || started.elapsed() > STARTUP_GRACE);
            // Finished only on the second look in a row: the runner republishes
            // the last text block as the run ends, and a turn that "ended" one
            // poll before its final words landed would lose them.
            let flush = over && over_before;
            over_before = over;
            let reply_to = self.reply_target(&*self.convs.lock().await, &conv);
            let n = snap.items.len();
            for (i, it) in snap.items.iter().enumerate() {
                if posted.contains(&it.id) {
                    continue;
                }
                match it.kind.as_str() {
                    "text" => {
                        let stable = last_text.get(&it.id) == Some(&it.text);
                        last_text.insert(it.id.clone(), it.text.clone());
                        // A block is done once something follows it and it
                        // stopped changing; anything still streaming holds back
                        // everything after it, so messages keep their order.
                        if !(flush || (i + 1 < n && stable)) {
                            break;
                        }
                        posted.insert(it.id.clone());
                        let body = for_chat(&it.text);
                        if !body.trim().is_empty() {
                            self.post(&conv, body, if said { None } else { reply_to.clone() });
                            said = true;
                        }
                    }
                    "notice" if it.data["level"].as_str() == Some("error") => {
                        posted.insert(it.id.clone());
                        self.post(&conv, format!("⚠️ {}", clip(&it.text, 1500)), None);
                        said = true;
                    }
                    "approval" | "question" => {
                        let pending = self
                            .h
                            .runtime(&task_id)
                            .pending
                            .lock()
                            .await
                            .contains_key(&it.id);
                        if !pending {
                            // Answered (or not registered yet). Leave it unposted
                            // until it is pending; once resolved it is skipped.
                            if it.data.get("resolved").is_some()
                                || it.data.get("answers").is_some()
                                || it.data.get("dismissed").is_some()
                            {
                                posted.insert(it.id.clone());
                            }
                            continue;
                        }
                        posted.insert(it.id.clone());
                        let questions =
                            it.data["questions"].as_array().cloned().unwrap_or_default();
                        let msg = if it.kind == "approval" {
                            render_approval(it)
                        } else {
                            render_question(it, &questions)
                        };
                        self.post(&conv, msg, reply_to.clone());
                        said = true;
                        let mut convs = self.convs.lock().await;
                        convs.entry(conv.clone()).or_default().awaiting = Some(Awaiting {
                            task_id: task_id.clone(),
                            item_id: it.id.clone(),
                            kind: it.kind.clone(),
                            questions,
                        });
                    }
                    _ => {}
                }
            }
            if snap.running && !asking && !over && last_typing.elapsed() >= TYPING_EVERY {
                let _ = self.out.send(Outgoing::Typing { conv: conv.clone() });
                last_typing = Instant::now();
            }
            if !flush {
                continue;
            }
            let mut convs = self.convs.lock().await;
            let c = convs.entry(conv.clone()).or_default();
            if c.gen != gen {
                // A message arrived while this poll ran: a new turn is starting.
                drop(convs);
                over_before = false;
                seen_running = false;
                started = Instant::now();
                continue;
            }
            c.watching = false;
            drop(convs);
            if !said {
                let msg = match snap.status.as_str() {
                    "failed" => {
                        "⚠️ The chat stopped with an error. Open it in OpenLeash for the details."
                    }
                    "stopped" => "⏹ Stopped.",
                    _ if !seen_running => {
                        "⚠️ The chat didn't start. Open it in OpenLeash to see why."
                    }
                    _ => "✅ Done.",
                };
                self.post(&conv, msg, reply_to);
            }
            break;
        }
        // The task vanished (deleted mid-turn): stop watching.
        if let Some(c) = self.convs.lock().await.get_mut(&conv) {
            c.watching = false;
        }
    }
}

/// The text the model receives: the person's words, then a reminder of where
/// they came from and what that means for the reply. The first message of a
/// chat carries the full guidance; later ones a one-line reminder, because a
/// long chat is compacted and must not lose the fact its replies leave the app.
fn frame(m: &Incoming, display: &str, platform: &str, first: bool) -> String {
    let who = format!(
        "{} (@{}, user id {}) in {}",
        platform, m.author, m.author_id, m.place
    );
    if first {
        let c = mark(platform);
        format!(
            "{display}\n\n<system-reminder>This chat is connected to {who}. The message above was sent there, and your replies are posted back to it as chat messages, so write for chat: short and direct, plain Markdown (no tables), and no `openleash-viz`, `openleash-chart`, `openleash-svg` or `openleash-image` blocks or other inline visualizations, which can't render there. The person may be away from this computer: your approvals and `ask_user` questions are relayed to them there and they answer by replying, so keep questions few and easy to answer in one line. Messages starting with `{c}new`, `{c}stop`, `{c}status` or `{c}help` are handled by the connector, not sent to you.</system-reminder>"
        )
    } else {
        format!("{display}\n\n<system-reminder>Sent from {who}; your reply is posted back there.</system-reminder>")
    }
}

#[cfg(test)]
mod tests {
    // `store::test_home` serializes `OPENLEASH_HOME` across tokio tests, so the
    // guard is deliberately held across `.await` points. See AGENTS.md: do not
    // drop the lock to quiet the lint.
    #![allow(clippy::await_holding_lock)]

    use super::*;

    fn q(kind: &str, opts: &[&str]) -> Value {
        json!({"question": "Which?", "type": kind, "options": opts.iter().map(|l| json!({"label": l})).collect::<Vec<_>>()})
    }

    #[test]
    fn commands_are_exact_words_only() {
        assert_eq!(command("/new"), Some(Cmd::New));
        assert_eq!(command("  !RESET "), Some(Cmd::New));
        assert_eq!(command("/stop"), Some(Cmd::Stop));
        assert_eq!(command("!status"), Some(Cmd::Status));
        assert_eq!(command("/help"), Some(Cmd::Help));
        assert_eq!(command("/newline"), None);
        assert_eq!(command("!new idea"), None);
        assert_eq!(command("new"), None);
        assert_eq!(command("/compact"), None, "backend commands go to the chat");
    }

    #[test]
    fn slack_is_told_the_bang_forms_because_it_swallows_slash_commands() {
        assert!(help("Discord").contains("`/new`"));
        assert!(help("Telegram").contains("`/stop`"));
        let slack = help("Slack");
        assert!(
            slack.contains("`!new`") && slack.contains("`!compact`"),
            "{slack}"
        );
        assert!(!slack.contains("`/"), "{slack}");
        let m = Incoming {
            conv: "C1:1.2".into(),
            message_id: "1.2".into(),
            author: "alice".into(),
            author_id: "U1".into(),
            place: "#general".into(),
            text: "hi".into(),
            attachments: vec![],
        };
        assert!(frame(&m, "hi", "Slack", true).contains("`!new`, `!stop`"));
        assert!(frame(&m, "hi", "Discord", true).contains("`/new`, `/stop`"));
    }

    #[test]
    fn backend_commands_accept_the_bang_form_for_exact_words_only() {
        assert_eq!(backend_command("/compact").as_deref(), Some("/compact"));
        assert_eq!(backend_command("!compact").as_deref(), Some("/compact"));
        assert_eq!(
            backend_command("!btw which file?").as_deref(),
            Some("/btw which file?")
        );
        assert_eq!(
            backend_command("!goal ship it").as_deref(),
            Some("/goal ship it")
        );
        assert_eq!(backend_command("!important: read this"), None);
        assert_eq!(backend_command("!compacting"), None);
        assert_eq!(backend_command("compact"), None);
        assert_eq!(backend_command("hello"), None);
    }

    #[test]
    fn only_a_clear_yes_approves() {
        assert_eq!(approval_reply("yes")["decision"], "once");
        assert_eq!(approval_reply("Yes!")["decision"], "once");
        assert_eq!(approval_reply("👍")["decision"], "once");
        assert_eq!(approval_reply("always")["decision"], "always");
        assert_eq!(approval_reply("no")["decision"], "deny");
        assert!(approval_reply("no").get("feedback").is_none());
        // Anything else is a deny that carries the words to the agent, never
        // an approval read out of a sentence that happens to start with "yes".
        let r = approval_reply("yes but first run the tests and then tell me what changed");
        assert_eq!(r["decision"], "deny");
        assert!(r["feedback"].as_str().unwrap().contains("run the tests"));
        let r = approval_reply("run the linter instead");
        assert_eq!(r["decision"], "deny");
        assert_eq!(r["feedback"], "run the linter instead");
        assert_eq!(approval_reply("")["decision"], "deny");
    }

    #[test]
    fn question_replies_map_onto_the_form() {
        let one = vec![q("single", &["Postgres", "SQLite"])];
        assert_eq!(question_reply(&one, "2")["answers"], json!(["SQLite"]));
        assert_eq!(
            question_reply(&one, "MySQL please")["answers"],
            json!(["MySQL please"])
        );
        assert_eq!(question_reply(&one, "skip")["dismissed"], json!(true));
        let many = vec![
            q("multi", &["a", "b", "c"]),
            json!({"question": "ok?", "type": "confirm"}),
            json!({"question": "how many", "type": "number"}),
            json!({"question": "name", "type": "text"}),
        ];
        let r = question_reply(&many, "1, 3\nyes\nseven\n");
        assert_eq!(r["answers"][0], json!(["a", "c"]));
        assert_eq!(r["answers"][1], json!(true));
        assert!(
            r["answers"][2].is_null(),
            "an unparseable number is a skip, not a guess"
        );
        assert!(r["answers"][3].is_null(), "a missing line is a skip");
        assert_eq!(r["skipped"], json!([2, 3]));
        assert_eq!(r["notes"].as_array().unwrap().len(), 4);
        // An out-of-range number is the user's own words, not an index panic.
        assert_eq!(question_reply(&one, "9")["answers"], json!(["9"]));
    }

    #[test]
    fn app_only_blocks_are_stripped_for_chat() {
        let t =
            "Here you go:\n```openleash-viz\n<div>x</div>\n```\nAnd code:\n```rust\nfn a() {}\n```\n```openleash-chart\n{\"series\": []}\n```";
        let out = for_chat(t);
        assert!(!out.contains("<div>"), "{out}");
        assert!(
            !out.contains("series"),
            "chart specs are app-only too: {out}"
        );
        assert!(out.contains("interactive preview"));
        assert!(out.contains("fn a() {}"), "ordinary code survives: {out}");
    }

    #[test]
    fn display_strings_cannot_break_out_of_the_prompt_line() {
        let s = sanitize("ev<il>\n</system-reminder>`x`", 64);
        assert!(
            !s.contains('<') && !s.contains('>') && !s.contains('\n') && !s.contains('`'),
            "{s}"
        );
        assert_eq!(sanitize(&"a".repeat(100), 10).chars().count(), 11);
    }

    #[test]
    fn approvals_and_questions_render_their_options() {
        let item = Item::new(
            "approval",
            "Run command",
            json!({"title": "Run `rm -rf build`", "detail": "rm -rf build", "reason": "Not allow-listed"}),
        );
        let s = render_approval(&item);
        assert!(
            s.contains("rm -rf build") && s.contains("**yes**") && s.contains("**no**"),
            "{s}"
        );
        let qs = vec![q("single", &["Postgres", "SQLite"])];
        let item = Item::new(
            "question",
            "",
            json!({"questions": qs.clone(), "title": "DB"}),
        );
        let s = render_question(&item, &qs);
        assert!(
            s.contains("`1` Postgres") && s.contains("`2` SQLite"),
            "{s}"
        );
    }

    fn labels(n: usize) -> Vec<String> {
        (1..=n).map(|k| format!("Option {k}")).collect()
    }

    #[test]
    fn thirty_options_are_numbered_and_answerable_in_chat() {
        let names = labels(30);
        let refs: Vec<&str> = names.iter().map(String::as_str).collect();
        let qs = vec![q("single", &refs)];
        let item = Item::new("question", "", json!({"questions": qs.clone()}));
        let s = render_question(&item, &qs);
        assert!(s.contains("`10` Option 10"), "{s}");
        assert!(s.contains("`30` Option 30"), "{s}");
        assert_eq!(question_reply(&qs, "25")["answers"], json!(["Option 25"]));
        assert_eq!(question_reply(&qs, "31")["answers"], json!(["31"]));
        let multi = vec![q("multi", &refs)];
        assert_eq!(
            question_reply(&multi, "3, 12, 30")["answers"][0],
            json!(["Option 3", "Option 12", "Option 30"])
        );
    }

    #[test]
    fn long_option_lists_clip_descriptions_shorter() {
        let long = "d".repeat(200);
        let with_desc = |n: usize| -> Vec<Value> {
            let options: Vec<Value> = labels(n)
                .into_iter()
                .map(|l| json!({"label": l, "description": long}))
                .collect();
            vec![json!({"question": "Which?", "type": "single", "options": options})]
        };
        let clipped = |n: usize| format!(" · {}…", "d".repeat(n));
        // Ten options keep the roomy clip.
        let qs = with_desc(10);
        let s = render_question(&Item::new("question", "", json!({})), &qs);
        assert!(s.contains(&clipped(160)), "{s}");
        // Past ten, each line is kept short so the form stays readable.
        let qs = with_desc(11);
        let s = render_question(&Item::new("question", "", json!({})), &qs);
        assert!(s.contains(&clipped(80)), "{s}");
        assert!(!s.contains(&"d".repeat(81)), "{s}");
    }

    #[test]
    fn the_first_message_carries_the_platform_rules() {
        let m = Incoming {
            conv: "1".into(),
            message_id: "2".into(),
            author: "alice".into(),
            author_id: "42".into(),
            place: "a direct message".into(),
            text: "hi".into(),
            attachments: vec![],
        };
        let first = frame(&m, "hi", "Discord", true);
        assert!(first.starts_with("hi\n"));
        assert!(first.contains("Discord (@alice, user id 42) in a direct message"));
        assert!(first.contains("openleash-viz"));
        assert!(first.contains("openleash-chart"));
        let later = frame(&m, "hi", "Discord", false);
        assert!(later.len() < first.len() && later.contains("posted back"));
    }

    use crate::agent::tests::{custom, harness, openai_ok, task, Mock};

    fn msg(conv: &str, id: &str, text: &str) -> Incoming {
        Incoming {
            conv: conv.into(),
            message_id: id.into(),
            author: "alice".into(),
            author_id: "100000000000000001".into(),
            place: "a direct message".into(),
            text: text.into(),
            attachments: vec![],
        }
    }

    fn defaults() -> ChatDefaults {
        ChatDefaults {
            platform: "Discord",
            project: String::new(),
            model: String::new(),
            perm: "auto".into(),
        }
    }

    async fn next_post(rx: &mut mpsc::UnboundedReceiver<Outgoing>) -> (String, Option<String>) {
        loop {
            match tokio::time::timeout(Duration::from_secs(20), rx.recv()).await {
                Ok(Some(Outgoing::Post { text, reply_to, .. })) => return (text, reply_to),
                Ok(Some(Outgoing::Typing { .. })) => continue,
                other => panic!("no post arrived: {other:?}"),
            }
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_message_runs_a_turn_in_its_linked_chat_and_the_reply_comes_back() {
        let dir = std::env::temp_dir().join(format!("ol-bridge-e2e-{}", std::process::id()));
        let _g = crate::agent::store::test_home(&dir);
        let m = Mock::start().await;
        let (cp, key) = custom("bridgeprov", &m.base, false);
        let mut s = crate::agent::store::Settings::default();
        s.custom_providers.push(cp);
        s.providers.insert(key.0, key.1);
        s.memory = false;
        let mut t = task("c1", "bridgeprov/m");
        t.status = "idle".into();
        let (h, _) = harness(s, vec![t]);
        let links = dir.join("links.json");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&links, r#"{"chan-1":"c1"}"#).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = Bridge::new(h.clone(), tx, links.clone());
        m.push(openai_ok("Fixed it. The build passes now."));
        b.handle(msg("chan-1", "m-1", "fix the build"), defaults())
            .await;
        let (text, reply_to) = next_post(&mut rx).await;
        assert_eq!(text, "Fixed it. The build passes now.");
        assert_eq!(
            reply_to.as_deref(),
            Some("m-1"),
            "the reply threads onto the message it answers"
        );
        // The model got the words plus where they came from; the transcript
        // shows only what the person typed.
        let req = m.reqs().last().cloned().expect("a model request");
        assert!(
            req.body.to_string().contains("Sent from Discord (@alice"),
            "{}",
            req.body
        );
        let t = h.task("c1").await.unwrap();
        let user = t
            .lock()
            .await
            .items
            .iter()
            .find(|i| i.kind == "user")
            .cloned()
            .unwrap();
        assert_eq!(user.text, "fix the build");
        // /new unlinks without touching the chat.
        b.handle(msg("chan-1", "m-2", "/new"), defaults()).await;
        let (text, _) = next_post(&mut rx).await;
        assert!(text.contains("Fresh start"));
        assert_eq!(b.chat_count().await, 0);
        assert!(std::fs::read_to_string(&links).unwrap().trim() == "{}");
        assert!(h.task("c1").await.is_ok());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_first_message_creates_a_chat_and_links_the_conversation() {
        let dir = std::env::temp_dir().join(format!("ol-bridge-create-{}", std::process::id()));
        let _g = crate::agent::store::test_home(&dir);
        let project = dir.join("proj");
        std::fs::create_dir_all(&project).unwrap();
        let m = Mock::start().await;
        let (cp, key) = custom("bridgenew", &m.base, false);
        let mut s = crate::agent::store::Settings::default();
        s.custom_providers.push(cp);
        s.providers.insert(key.0, key.1);
        s.memory = false;
        let (h, events) = harness(s, vec![]);
        let links = dir.join("links.json");
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = Bridge::new(h.clone(), tx, links.clone());
        m.push(openai_ok("Hi! What should we work on?"));
        let d = ChatDefaults {
            project: project.to_string_lossy().into_owned(),
            model: "bridgenew/m".into(),
            ..defaults()
        };
        b.handle(msg("chan-9", "m-1", "hello there"), d).await;
        let (text, _) = next_post(&mut rx).await;
        assert_eq!(text, "Hi! What should we work on?");
        let saved: HashMap<String, String> =
            serde_json::from_str(&std::fs::read_to_string(&links).unwrap()).unwrap();
        let id = saved
            .get("chan-9")
            .cloned()
            .expect("the conversation is linked on disk");
        let t = h.task(&id).await.unwrap();
        let t = t.lock().await;
        assert_eq!(
            t.title, "hello there",
            "the title is the person's words, not the framing"
        );
        assert_eq!(
            t.perm, "auto",
            "the connector's permission mode, not the app default"
        );
        assert!(t
            .messages
            .first()
            .is_some_and(|m| serde_json::to_string(&m.content)
                .unwrap()
                .contains("This chat is connected to Discord")));
        // The app is told about a chat it did not create.
        assert!(events
            .lock()
            .unwrap()
            .iter()
            .any(|(n, v)| n == "ol://event" && v["kind"] == "task" && v["task_id"] == id));
        drop(t);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn a_relayed_approval_is_answered_by_the_next_reply() {
        let dir = std::env::temp_dir().join(format!("ol-bridge-approve-{}", std::process::id()));
        let _g = crate::agent::store::test_home(&dir);
        let mut t = task("c2", "x/m");
        t.status = "waiting".into();
        t.waiting_kind = Some("approval".into());
        let card = Item::new(
            "approval",
            "Run command",
            json!({"kind": "command", "title": "Run `cargo publish`", "detail": "cargo publish"}),
        );
        t.items.push(card.clone());
        let (h, _) = harness(crate::agent::store::Settings::default(), vec![t]);
        let (resp_tx, resp_rx) = tokio::sync::oneshot::channel();
        h.runtime("c2")
            .pending
            .lock()
            .await
            .insert(card.id.clone(), resp_tx);
        h.runtime("c2").running.store(true, Ordering::SeqCst);
        std::fs::create_dir_all(&dir).unwrap();
        let links = dir.join("links.json");
        std::fs::write(&links, r#"{"chan-2":"c2"}"#).unwrap();
        let (tx, mut rx) = mpsc::unbounded_channel();
        let b = Bridge::new(h.clone(), tx, links);
        let since = card.ts - chrono::Duration::seconds(1);
        b.watch("chan-2", "c2", since, "m-1".into()).await;
        let (text, _) = next_post(&mut rx).await;
        assert!(
            text.contains("Approval needed") && text.contains("cargo publish"),
            "{text}"
        );
        b.handle(msg("chan-2", "m-2", "yes"), defaults()).await;
        let got = tokio::time::timeout(Duration::from_secs(5), resp_rx)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(got, json!({"decision": "once"}));
        // The reply was the answer, not a new message for the agent.
        let t = h.task("c2").await.unwrap();
        assert!(!t.lock().await.items.iter().any(|i| i.kind == "user"));
        h.runtime("c2").running.store(false, Ordering::SeqCst);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
