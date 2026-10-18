//! The user-driven half of the browser: the Browser view beside the chat.
//!
//! These are narrow, explicit user controls over the main agent's existing
//! session. No caller-supplied JavaScript or CDP method ever crosses this
//! boundary: the webview sends a closed `PanelAction`, validated here, and the
//! backend decides what runs in the page. The one place page script runs on the
//! user's behalf (`preview_style`) is a fixed function whose arguments are
//! whitelisted, validated and passed JSON-encoded — never spliced into code.

use super::browser::{DialogInfo, ElementInfo, Session, TabInfo};
use super::browser_policy::{classify_host, HostClass};
use serde::{Deserialize, Serialize};
use serde_json::json;

/// Most visited URLs a frame carries for the address bar's suggestions.
pub const HISTORY_LIMIT: usize = 50;
/// Largest wheel step, in CSS pixels, either axis.
const MAX_SCROLL: f64 = 2000.0;
/// Most style properties one preview may set.
const MAX_STYLES: usize = 20;
/// Longest style value. Enough for a font stack, far too short for a payload.
const MAX_STYLE_VALUE: usize = 200;
/// Longest selector a preview may target.
const MAX_SELECTOR: usize = 1000;

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PanelAction {
    Snapshot {},
    Navigate {
        url: String,
    },
    Click {
        x: f64,
        y: f64,
        #[serde(default)]
        button: Option<MouseButton>,
        #[serde(default)]
        click_count: Option<u8>,
    },
    Type {
        text: String,
    },
    Key {
        key: PanelKey,
        #[serde(default)]
        modifiers: Modifiers,
    },
    Scroll {
        delta_y: f64,
        #[serde(default)]
        delta_x: Option<f64>,
        #[serde(default)]
        x: Option<f64>,
        #[serde(default)]
        y: Option<f64>,
    },
    Back {},
    Forward {},
    Refresh {},
    TabNew {
        #[serde(default)]
        url: Option<String>,
    },
    TabSwitch {
        id: String,
    },
    TabClose {
        id: String,
    },
    Dialog {
        accept: bool,
        #[serde(default)]
        text: Option<String>,
    },
    /// What is under the pointer, for Annotate. Read-only in the page.
    Inspect {
        x: f64,
        y: f64,
    },
    /// Inline styles on one element, as a live preview the user is tuning.
    PreviewStyle {
        selector: String,
        styles: Vec<StyleChange>,
    },
    /// Undo every preview this panel applied.
    ClearPreview {},
}

#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

/// The keys the panel forwards on their own. Printable text goes through
/// `type`, which is not at the mercy of the OS keymap; anything else (a raw
/// key code, a key name the page invents) has no way in.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq, Eq)]
pub enum PanelKey {
    Enter,
    Tab,
    Backspace,
    Delete,
    Escape,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Space,
}

impl PanelKey {
    /// CDP `key`, `code`, Windows virtual key code, and the text a plain press
    /// produces (Enter's `\r` is what makes a form submit implicitly).
    fn cdp(self) -> (&'static str, &'static str, u32, Option<&'static str>) {
        match self {
            Self::Enter => ("Enter", "Enter", 13, Some("\r")),
            Self::Tab => ("Tab", "Tab", 9, None),
            Self::Backspace => ("Backspace", "Backspace", 8, None),
            Self::Delete => ("Delete", "Delete", 46, None),
            Self::Escape => ("Escape", "Escape", 27, None),
            Self::ArrowUp => ("ArrowUp", "ArrowUp", 38, None),
            Self::ArrowDown => ("ArrowDown", "ArrowDown", 40, None),
            Self::ArrowLeft => ("ArrowLeft", "ArrowLeft", 37, None),
            Self::ArrowRight => ("ArrowRight", "ArrowRight", 39, None),
            Self::Home => ("Home", "Home", 36, None),
            Self::End => ("End", "End", 35, None),
            Self::PageUp => ("PageUp", "PageUp", 33, None),
            Self::PageDown => ("PageDown", "PageDown", 34, None),
            Self::Space => (" ", "Space", 32, Some(" ")),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, Deserialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Modifiers {
    pub ctrl: bool,
    pub shift: bool,
    pub alt: bool,
    pub meta: bool,
}

impl Modifiers {
    /// CDP's modifier bitmask: Alt 1, Ctrl 2, Meta 4, Shift 8.
    fn mask(self) -> u32 {
        (self.alt as u32)
            | (self.ctrl as u32) << 1
            | (self.meta as u32) << 2
            | (self.shift as u32) << 3
    }
    /// A chord (Ctrl/Alt/Meta held) produces no text, only the shortcut.
    fn chord(self) -> bool {
        self.ctrl || self.alt || self.meta
    }
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct StyleChange {
    pub property: String,
    pub value: String,
}

/// The properties Annotate can adjust. A closed list rather than "anything
/// that parses": `background-image`, `content`, `behavior`, custom
/// properties and friends can fetch URLs or smuggle content, and the panel has
/// no use for them.
pub const STYLE_PROPERTIES: &[&str] = &[
    "font-size",
    "font-weight",
    "font-family",
    "line-height",
    "letter-spacing",
    "text-align",
    "color",
    "background-color",
    "padding",
    "padding-top",
    "padding-right",
    "padding-bottom",
    "padding-left",
    "margin",
    "margin-top",
    "margin-right",
    "margin-bottom",
    "margin-left",
    "gap",
    "border-radius",
    "border-color",
    "border-width",
    "width",
    "height",
    "opacity",
];

pub(crate) fn validate_style_property(property: &str) -> Result<(), String> {
    if STYLE_PROPERTIES.contains(&property) {
        Ok(())
    } else {
        Err(format!(
            "browser: `{}` is not a style the panel can adjust",
            property.chars().take(60).collect::<String>()
        ))
    }
}

/// A style value is a short, plain CSS token list: no URL loads, no legacy
/// script hooks, nothing that could close the declaration or the rule. The
/// value is also only ever handed to `style.setProperty`, so this is defence
/// in depth, not the only thing standing between the page and a payload.
pub(crate) fn validate_style_value(value: &str, allow_empty: bool) -> Result<(), String> {
    let v = value.trim();
    if v.is_empty() {
        return if allow_empty {
            Ok(())
        } else {
            Err("browser: a style value is empty".into())
        };
    }
    if value.len() > MAX_STYLE_VALUE {
        return Err(format!(
            "browser: a style value is longer than {MAX_STYLE_VALUE} bytes"
        ));
    }
    if value.chars().any(|c| c.is_control()) {
        return Err("browser: a style value contains a control character".into());
    }
    let lower = v.to_ascii_lowercase();
    for bad in [
        "url(",
        "expression",
        "javascript:",
        "image-set(",
        "image(",
        "element(",
        "/*",
    ] {
        if lower.contains(bad) {
            return Err(format!("browser: `{bad}` is not allowed in a style value"));
        }
    }
    if let Some(c) = v
        .chars()
        .find(|c| matches!(c, ';' | '{' | '}' | '<' | '>' | '\\' | '@'))
    {
        return Err(format!("browser: `{c}` is not allowed in a style value"));
    }
    Ok(())
}

pub(crate) fn validate_styles(styles: &[StyleChange]) -> Result<(), String> {
    if styles.is_empty() || styles.len() > MAX_STYLES {
        return Err(format!(
            "browser: a preview sets 1–{MAX_STYLES} style properties"
        ));
    }
    for s in styles {
        validate_style_property(&s.property)?;
        validate_style_value(&s.value, false)?;
    }
    Ok(())
}

fn validate_point(x: f64, y: f64, what: &str) -> Result<(), String> {
    if !x.is_finite() || !y.is_finite() || x < 0.0 || y < 0.0 {
        return Err(format!("browser: invalid {what} coordinates"));
    }
    Ok(())
}

fn validate_tab_id(id: &str) -> Result<(), String> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err("browser: invalid tab id".into());
    }
    Ok(())
}

fn validate_selector(selector: &str) -> Result<(), String> {
    if selector.trim().is_empty()
        || selector.len() > MAX_SELECTOR
        || selector.chars().any(|c| c.is_control())
    {
        return Err("browser: invalid element selector".into());
    }
    Ok(())
}

impl PanelAction {
    pub fn validate(&self) -> Result<(), String> {
        match self {
            Self::Snapshot {}
            | Self::Back {}
            | Self::Forward {}
            | Self::Refresh {}
            | Self::ClearPreview {}
            | Self::Key { .. } => Ok(()),
            Self::Navigate { url } => validate_panel_url(url),
            Self::TabNew { url } => url.as_deref().map_or(Ok(()), validate_panel_url),
            Self::Click {
                x, y, click_count, ..
            } => {
                validate_point(*x, *y, "click")?;
                if click_count.is_some_and(|n| !(1..=3).contains(&n)) {
                    return Err("browser: click count must be 1–3".into());
                }
                Ok(())
            }
            Self::Type { text } if text.is_empty() || text.len() > 100_000 => {
                Err("browser: text must contain 1–100000 bytes".into())
            }
            Self::Type { .. } => Ok(()),
            Self::Scroll {
                delta_y,
                delta_x,
                x,
                y,
            } => {
                let dx = delta_x.unwrap_or(0.0);
                if !delta_y.is_finite()
                    || !dx.is_finite()
                    || delta_y.abs() > MAX_SCROLL
                    || dx.abs() > MAX_SCROLL
                {
                    return Err("browser: invalid scroll distance".into());
                }
                if x.is_some() || y.is_some() {
                    validate_point(x.unwrap_or(0.0), y.unwrap_or(0.0), "scroll")?;
                }
                Ok(())
            }
            Self::TabSwitch { id } | Self::TabClose { id } => validate_tab_id(id),
            Self::Dialog { text, .. } if text.as_ref().is_some_and(|t| t.len() > 10_000) => {
                Err("browser: dialog text is longer than 10000 bytes".into())
            }
            Self::Dialog { .. } => Ok(()),
            Self::Inspect { x, y } => validate_point(*x, *y, "inspect"),
            Self::PreviewStyle { selector, styles } => {
                validate_selector(selector)?;
                validate_styles(styles)
            }
        }
    }
}

pub(crate) fn validate_panel_url(url: &str) -> Result<(), String> {
    let parsed = reqwest::Url::parse(url).map_err(|_| "browser: invalid URL")?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err("browser: only HTTP and HTTPS URLs are allowed".into());
    }
    Ok(())
}

/// What a URL the user typed means for the network policy: a LAN host they
/// typed is their approval for it, a blocked one is refused outright.
#[derive(Debug, PartialEq, Eq)]
enum TypedHost {
    Plain,
    Lan(String),
}

fn typed_host(url: &str) -> Result<TypedHost, String> {
    validate_panel_url(url)?;
    let parsed = reqwest::Url::parse(url).map_err(|_| "browser: invalid URL")?;
    let host = parsed.host_str().unwrap_or_default().to_ascii_lowercase();
    match classify_host(&host) {
        HostClass::Blocked => Err(format!(
            "browser: {host} is never reachable from the browser (cloud metadata, link-local or reserved)"
        )),
        HostClass::Lan => Ok(TypedHost::Lan(host)),
        HostClass::Loopback | HostClass::Public => Ok(TypedHost::Plain),
    }
}

async fn approve_typed(s: &Session, url: &str) -> Result<(), String> {
    if let TypedHost::Lan(host) = typed_host(url)? {
        s.allow_lan_host(&host);
    }
    Ok(())
}

#[derive(Debug, Serialize, Default)]
pub struct PanelFrame {
    pub url: String,
    pub title: String,
    /// Base64 PNG. Empty when no fresh picture could be taken (a JavaScript
    /// dialog blocks the page's renderer): the panel keeps its last one.
    pub png: String,
    pub width: u32,
    pub height: u32,
    pub viewport_width: f64,
    pub viewport_height: f64,
    pub tabs: Vec<TabInfo>,
    pub can_back: bool,
    pub can_forward: bool,
    /// Visited URLs, newest first, at most `HISTORY_LIMIT`.
    pub history: Vec<String>,
    pub dialog: Option<DialogInfo>,
    /// The element an `inspect` found.
    pub element: Option<ElementInfo>,
}

/// The fixed preview function. `sel` and `styles` arrive as JSON literals; the
/// original `style` attribute is kept per element so `clear_preview` restores
/// exactly what the page had, not what the panel guessed.
const PREVIEW_JS: &str = r#"((sel, styles) => {
  const el = document.querySelector(sel);
  if (!el || !el.style) return "missing";
  const kept = window.__openleashPreview || (window.__openleashPreview = new Map());
  if (!kept.has(el)) kept.set(el, el.getAttribute("style"));
  for (const [p, v] of styles) el.style.setProperty(p, v);
  return "ok";
})"#;

const CLEAR_PREVIEW_JS: &str = r#"(() => {
  const kept = window.__openleashPreview;
  if (!kept) return 0;
  let n = 0;
  for (const [el, style] of kept) {
    if (style === null) el.removeAttribute("style"); else el.setAttribute("style", style);
    n++;
  }
  kept.clear();
  return n;
})()"#;

/// The preview call, with every argument JSON-encoded.
pub(crate) fn preview_expression(selector: &str, styles: &[StyleChange]) -> String {
    let pairs: Vec<[&str; 2]> = styles
        .iter()
        .map(|s| [s.property.as_str(), s.value.trim()])
        .collect();
    format!(
        "{PREVIEW_JS}({}, {})",
        serde_json::to_string(selector).unwrap_or_else(|_| "\"\"".into()),
        serde_json::to_string(&pairs).unwrap_or_else(|_| "[]".into())
    )
}

async fn viewport(s: &Session) -> Result<(f64, f64), String> {
    let metrics = s.raw("Page.getLayoutMetrics", json!({})).await?;
    let v = &metrics["cssVisualViewport"];
    Ok((
        v["clientWidth"].as_f64().unwrap_or(0.0),
        v["clientHeight"].as_f64().unwrap_or(0.0),
    ))
}

async fn inside_viewport(s: &Session, x: f64, y: f64, what: &str) -> Result<(), String> {
    let (w, h) = viewport(s).await?;
    if x >= w || y >= h {
        return Err(format!("browser: {what} is outside the viewport"));
    }
    Ok(())
}

/// Run one validated user action against `s`, then return a fresh frame.
pub async fn run(s: &Session, action: PanelAction) -> Result<PanelFrame, String> {
    action.validate()?;
    let mut element = None;
    match action {
        PanelAction::Snapshot {} => {}
        PanelAction::Navigate { url } => {
            approve_typed(s, &url).await?;
            s.goto(&url).await?;
        }
        PanelAction::Click {
            x,
            y,
            button,
            click_count,
        } => {
            inside_viewport(s, x, y, "click").await?;
            let button = match button.unwrap_or(MouseButton::Left) {
                MouseButton::Left => "left",
                MouseButton::Middle => "middle",
                MouseButton::Right => "right",
            };
            let count = click_count.unwrap_or(1);
            // Moving first gives the page its hover state, which is what a
            // menu that opens on hover needs before the press lands.
            s.raw(
                "Input.dispatchMouseEvent",
                json!({"type": "mouseMoved", "x": x, "y": y}),
            )
            .await?;
            for event in ["mousePressed", "mouseReleased"] {
                s.raw(
                    "Input.dispatchMouseEvent",
                    json!({"type": event, "x": x, "y": y, "button": button, "clickCount": count}),
                )
                .await?;
            }
            s.settle().await?;
        }
        PanelAction::Type { text } => {
            s.type_text(&text).await?;
        }
        PanelAction::Key { key, modifiers } => {
            let (name, code, vk, text) = key.cdp();
            let text = if modifiers.chord() { None } else { text };
            let mut down = json!({
                "type": if text.is_some() { "keyDown" } else { "rawKeyDown" },
                "key": name, "code": code,
                "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk,
                "modifiers": modifiers.mask(),
            });
            if let Some(t) = text {
                down["text"] = json!(t);
                down["unmodifiedText"] = json!(t);
            }
            s.raw("Input.dispatchKeyEvent", down).await?;
            s.raw(
                "Input.dispatchKeyEvent",
                json!({"type": "keyUp", "key": name, "code": code, "windowsVirtualKeyCode": vk, "nativeVirtualKeyCode": vk, "modifiers": modifiers.mask()}),
            )
            .await?;
            s.settle().await?;
        }
        PanelAction::Scroll {
            delta_y,
            delta_x,
            x,
            y,
        } => {
            let (w, h) = viewport(s).await?;
            // The wheel lands where the pointer is, so a scrollable panel under
            // it scrolls rather than the page; the centre when not said.
            let px = x.unwrap_or(w / 2.0).min((w - 1.0).max(0.0));
            let py = y.unwrap_or(h / 2.0).min((h - 1.0).max(0.0));
            s.raw(
                "Input.dispatchMouseEvent",
                json!({"type":"mouseWheel","x":px,"y":py,"deltaX":delta_x.unwrap_or(0.0),"deltaY":delta_y}),
            )
            .await?;
        }
        PanelAction::Back {} => {
            s.go_back().await?;
            s.settle().await?;
        }
        PanelAction::Forward {} => {
            s.go_forward().await?;
            s.settle().await?;
        }
        PanelAction::Refresh {} => {
            s.reload().await?;
            s.settle().await?;
        }
        PanelAction::TabNew { url } => {
            if let Some(u) = url.as_deref() {
                approve_typed(s, u).await?;
            }
            s.new_tab(url.as_deref()).await?;
            s.settle().await?;
        }
        PanelAction::TabSwitch { id } => {
            s.switch_tab(&id).await?;
        }
        PanelAction::TabClose { id } => {
            s.close_tab(&id).await?;
        }
        PanelAction::Dialog { accept, text } => {
            s.answer_dialog(accept, text.as_deref()).await?;
            s.settle().await?;
        }
        PanelAction::Inspect { x, y } => {
            inside_viewport(s, x, y, "the point").await?;
            element = Some(s.element_at(x, y).await?);
        }
        PanelAction::PreviewStyle { selector, styles } => {
            let r = s.eval(&preview_expression(&selector, &styles)).await?;
            if r.as_str() != Some("ok") {
                return Err("browser: that element is no longer on the page".into());
            }
        }
        PanelAction::ClearPreview {} => {
            s.eval(CLEAR_PREVIEW_JS).await?;
        }
    }
    let mut frame = frame(s).await?;
    frame.element = element;
    Ok(frame)
}

/// The panel's picture of the session: the active tab's page plus its chrome.
pub async fn frame(s: &Session) -> Result<PanelFrame, String> {
    // Asked first: everything after it may need a renderer the dialog blocks.
    let dialog = s.pending_dialog().await;
    let tabs = s.tabs().await.unwrap_or_default();
    let (can_back, can_forward) = s.can_go().await;
    let mut history = s.history().await;
    history.truncate(HISTORY_LIMIT);
    let active = tabs.iter().find(|t| t.active).cloned();
    // An open alert/confirm/prompt blocks the renderer: evaluating or capturing
    // would sit out the call timeout. Answer with the chrome and the dialog,
    // and let the panel keep its last picture until the user answers it.
    if let Some(dialog) = dialog {
        let active = active.unwrap_or_default();
        return Ok(PanelFrame {
            url: active.url,
            title: active.title,
            tabs,
            can_back,
            can_forward,
            history,
            dialog: Some(dialog),
            ..PanelFrame::default()
        });
    }
    let state = s.state().await?;
    let bytes = s.screenshot(false).await?;
    let image =
        image::load_from_memory(&bytes).map_err(|e| format!("browser: invalid screenshot: {e}"))?;
    let (vw, vh) = viewport(s).await.unwrap_or((0.0, 0.0));
    Ok(PanelFrame {
        url: state.url,
        title: state.title,
        png: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes),
        width: image.width(),
        height: image.height(),
        viewport_width: if vw > 0.0 { vw } else { image.width() as f64 },
        viewport_height: if vh > 0.0 { vh } else { image.height() as f64 },
        tabs,
        can_back,
        can_forward,
        history,
        dialog: None,
        element: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(v: serde_json::Value) -> Result<PanelAction, String> {
        serde_json::from_value::<PanelAction>(v).map_err(|e| e.to_string())
    }

    #[test]
    fn panel_validation_is_narrow() {
        for url in [
            "file:///secret",
            "javascript:alert(1)",
            "data:text/html,x",
            "about:blank",
            "chrome://settings",
            "not a url",
        ] {
            assert!(PanelAction::Navigate { url: url.into() }
                .validate()
                .is_err());
            assert!(PanelAction::TabNew {
                url: Some(url.into())
            }
            .validate()
            .is_err());
        }
        assert!(PanelAction::Navigate {
            url: "http://localhost:3000/".into()
        }
        .validate()
        .is_ok());
        assert!(PanelAction::TabNew { url: None }.validate().is_ok());
        let click = |x: f64, y: f64, n: Option<u8>| PanelAction::Click {
            x,
            y,
            button: None,
            click_count: n,
        };
        assert!(click(f64::NAN, 0.0, None).validate().is_err());
        assert!(click(-1.0, 0.0, None).validate().is_err());
        assert!(click(0.0, f64::INFINITY, None).validate().is_err());
        assert!(click(1.0, 1.0, Some(0)).validate().is_err());
        assert!(click(1.0, 1.0, Some(4)).validate().is_err());
        assert!(click(1.0, 1.0, Some(2)).validate().is_ok());
        assert!(PanelAction::Type {
            text: String::new()
        }
        .validate()
        .is_err());
        let scroll = |dy: f64, dx: Option<f64>, x: Option<f64>| PanelAction::Scroll {
            delta_y: dy,
            delta_x: dx,
            x,
            y: Some(1.0),
        };
        assert!(scroll(2001.0, None, None).validate().is_err());
        assert!(scroll(10.0, Some(f64::NAN), None).validate().is_err());
        assert!(scroll(10.0, Some(-2500.0), None).validate().is_err());
        assert!(scroll(10.0, None, Some(-3.0)).validate().is_err());
        assert!(scroll(-400.0, Some(20.0), Some(3.0)).validate().is_ok());
        for id in ["", "a b", "x\"y", &"t".repeat(129), "../tab"] {
            assert!(
                PanelAction::TabSwitch { id: id.into() }.validate().is_err(),
                "{id}"
            );
            assert!(PanelAction::TabClose { id: id.into() }.validate().is_err());
        }
        assert!(PanelAction::TabSwitch {
            id: "8F2A-tab_1".into()
        }
        .validate()
        .is_ok());
        assert!(PanelAction::Dialog {
            accept: true,
            text: Some("x".repeat(10_001))
        }
        .validate()
        .is_err());
        assert!(PanelAction::Inspect { x: -0.5, y: 2.0 }.validate().is_err());
        assert!(PanelAction::Inspect { x: 0.0, y: 2.0 }.validate().is_ok());
    }

    #[test]
    fn deserialisation_rejects_unknown_kinds_fields_and_keys() {
        for bad in [
            json!({"kind":"js","expression":"alert(1)"}),
            json!({"kind":"cdp","method":"Runtime.evaluate"}),
            json!({"kind":"snapshot","owner":"other"}),
            json!({"kind":"click","x":1,"y":1,"selector":"#a"}),
            json!({"kind":"click","x":1,"y":1,"button":"back"}),
            json!({"kind":"key","key":"F12"}),
            json!({"kind":"key","key":"a"}),
            json!({"kind":"key","key":"enter"}),
            json!({"kind":"key","key":"Enter","modifiers":{"ctrl":true,"hyper":true}}),
            json!({"kind":"key","key":"Enter","code":13}),
            json!({"kind":"preview_style","selector":"a","styles":[{"property":"color","value":"red","important":true}]}),
            json!({"kind":"tab_new","url":"https://a.test","background":true}),
            json!({"kind":"dialog"}),
        ] {
            assert!(parse(bad.clone()).is_err(), "accepted {bad}");
        }
        for good in [
            json!({"kind":"snapshot"}),
            json!({"kind":"forward"}),
            json!({"kind":"clear_preview"}),
            json!({"kind":"click","x":1,"y":2,"button":"right","click_count":2}),
            json!({"kind":"key","key":"ArrowDown","modifiers":{"shift":true}}),
            json!({"kind":"key","key":"Space"}),
            json!({"kind":"scroll","delta_y":120}),
            json!({"kind":"tab_new"}),
            json!({"kind":"tab_switch","id":"t2"}),
            json!({"kind":"dialog","accept":false}),
            json!({"kind":"inspect","x":3,"y":4}),
        ] {
            assert!(parse(good.clone()).is_ok(), "rejected {good}");
        }
    }

    #[test]
    fn keys_map_to_cdp_with_modifiers() {
        assert_eq!(PanelKey::Enter.cdp(), ("Enter", "Enter", 13, Some("\r")));
        assert_eq!(PanelKey::Space.cdp().1, "Space");
        assert_eq!(PanelKey::PageDown.cdp().2, 34);
        let m = Modifiers {
            ctrl: true,
            shift: true,
            alt: false,
            meta: false,
        };
        assert_eq!(m.mask(), 2 | 8);
        assert!(m.chord());
        assert!(!Modifiers {
            shift: true,
            ..Default::default()
        }
        .chord());
        assert_eq!(
            Modifiers {
                alt: true,
                meta: true,
                ..Default::default()
            }
            .mask(),
            1 | 4
        );
    }

    #[test]
    fn style_previews_are_whitelisted_and_validated() {
        let one = |p: &str, v: &str| PanelAction::PreviewStyle {
            selector: "main > h1".into(),
            styles: vec![StyleChange {
                property: p.into(),
                value: v.into(),
            }],
        };
        for (p, v) in [
            ("font-size", "18px"),
            ("font-family", "\"Inter\", system-ui, sans-serif"),
            ("color", "rgb(10, 20, 30)"),
            ("background-color", "#fff"),
            ("padding", "4px 8px"),
            ("opacity", "0.5"),
            ("width", "calc(100% - 2rem)"),
        ] {
            assert!(one(p, v).validate().is_ok(), "{p}: {v}");
        }
        for (p, v) in [
            ("background-image", "none"),
            ("content", "'x'"),
            ("--x", "1"),
            ("behavior", "a"),
            ("position", "fixed"),
            ("color", ""),
            ("color", "red; background: url(x)"),
            ("color", "URL(https://evil.test)"),
            ("color", "expression(alert(1))"),
            ("color", "javascript:alert(1)"),
            ("color", "red}body{color:blue"),
            ("color", "red{"),
            ("color", "<script>"),
            ("color", "\\72 ed"),
            ("color", "@import"),
            ("color", "red\n"),
            ("color", "red /* x */"),
            ("background-color", "image-set(\"a.png\" 1x)"),
        ] {
            assert!(one(p, v).validate().is_err(), "accepted {p}: {v:?}");
        }
        assert!(one("color", &"a".repeat(201)).validate().is_err());
        assert!(PanelAction::PreviewStyle {
            selector: "a".into(),
            styles: vec![]
        }
        .validate()
        .is_err());
        assert!(PanelAction::PreviewStyle {
            selector: "a".into(),
            styles: (0..21)
                .map(|_| StyleChange {
                    property: "color".into(),
                    value: "red".into()
                })
                .collect()
        }
        .validate()
        .is_err());
        for sel in ["", "  ", "a\nb", &"a".repeat(1001)] {
            assert!(PanelAction::PreviewStyle {
                selector: sel.into(),
                styles: vec![StyleChange {
                    property: "color".into(),
                    value: "red".into()
                }]
            }
            .validate()
            .is_err());
        }
    }

    #[test]
    fn preview_arguments_are_json_encoded_not_spliced() {
        let expr = preview_expression(
            "a[title=\"x\"]'); alert(1); ('",
            &[StyleChange {
                property: "color".into(),
                value: " red ".into(),
            }],
        );
        assert!(expr.starts_with(PREVIEW_JS));
        // The selector arrives as one JSON string literal, quotes escaped.
        assert!(expr.contains(r#"("a[title=\"x\"]'); alert(1); ('", [["color","red"]])"#));
        assert!(!expr.contains("\nalert"));
    }

    #[test]
    fn typed_hosts_follow_the_network_policy() {
        assert_eq!(
            typed_host("http://localhost:3000/").unwrap(),
            TypedHost::Plain
        );
        assert_eq!(
            typed_host("https://example.com/").unwrap(),
            TypedHost::Plain
        );
        assert!(typed_host("http://metadata.google.internal/").is_err());
        assert!(typed_host("file:///etc/passwd").is_err());
        // RFC1918 is LAN under every version of the policy: the user typed it,
        // so it comes back as a host to approve rather than a refusal.
        assert_eq!(
            typed_host("http://192.168.1.20:8080/").unwrap(),
            TypedHost::Lan("192.168.1.20".into())
        );
    }

    #[test]
    fn frames_serialise_the_chrome_the_panel_needs() {
        let f = PanelFrame {
            tabs: vec![TabInfo {
                id: "t1".into(),
                url: "https://a.test/".into(),
                title: "A".into(),
                active: true,
            }],
            can_back: true,
            history: vec!["https://a.test/".into()],
            ..Default::default()
        };
        let v = serde_json::to_value(&f).unwrap();
        for key in [
            "url",
            "title",
            "png",
            "width",
            "height",
            "viewport_width",
            "viewport_height",
            "tabs",
            "can_back",
            "can_forward",
            "history",
            "dialog",
            "element",
        ] {
            assert!(v.get(key).is_some(), "missing {key}");
        }
        assert_eq!(v["tabs"][0]["active"], true);
        assert!(v["dialog"].is_null());
    }
}
