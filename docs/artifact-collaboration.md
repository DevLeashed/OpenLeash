# Collaborating with artifacts

Artifacts are optional project working surfaces. Ordinary chat still works. Open **Artifacts** to keep chat and the composer beside a versioned design, prototype, architecture map or investigation. Narrow windows stack the panes.

## User workflow

1. Ask the agent to inspect your project and create a purposeful artifact. The searchable list shows format, versions, unresolved requests and a decision excerpt. SVG and embedded screenshot records have inert image thumbnails; HTML does not run just to generate a thumbnail.
2. Choose an exact version. **Compare versions** shows both previews and a source diff. Old discussions remain in the all-revision feedback history.
3. Point at the issue: select text in the host preview and use **Use selected text as target**; click a marked HTML target; select an architecture component/dependency; or open **Select screenshot region** on a visual-investigation record and set its percentage rectangle. Region controls work with the keyboard and preserve original image bytes.
4. Preview proposals are untrusted private drafts. Open **Advanced feedback**, review the proposal into the fields, add your note, optionally save/select an annotation, and inspect the state to be included.
5. Click **Send feedback to agent**. Only this explicit action shares selected notes/state. Failed sends retain your fields. Switching versions retains separate drafts while the workspace is open; closing it discards unsent local drafts.
6. **Implement this revision**, **Explore project architecture**, and **Investigate visual issue** only stage editable requests. Review and send them. They do not grant permissions, execute commands or prove implementation.
7. The agent revises against the current parent version, responds to your submitted feedback, and records changed-file references, verification commands/scope/results and limitations. Read these as evidence claims and inspect the receipts/output when confidence matters.

## Agent formats

Normal HTML/SVG/Markdown/JSON artifacts remain supported. HTML stays in the existing isolated, network-disabled runtime. Use `data-openleash-target="Clear node label"` for draft-only click targets. Local interactive choices may propose state using the existing bounded draft protocol; never claim they already applied to the project.

For a grounded architecture map or visual investigation, create a **JSON artifact** with this schema:

```json
{
  "schema": "openleash.collaboration.v1",
  "title": "Project architecture",
  "purpose": "Explain the files actually inspected",
  "stage": "prototype",
  "nodes": [
    {"id": "ui", "title": "UI", "description": "Renders project state", "refs": [{"path": "src/App.tsx", "line": 1}]},
    {"id": "core", "title": "Core", "description": "Handles operations", "refs": []}
  ],
  "edges": [{"from": "ui", "to": "core", "reason": "Observed call boundary"}],
  "evidence": [{"command": "npm test -- example.test.ts", "result": "12 passed", "scope": "Filtered fixture", "path": "Logs/results.txt"}],
  "limitations": ["Native behavior not tested"]
}
```

Stages are `concept`, `prototype`, `implemented`, or `verified`. The UI explicitly calls these **recorded stages**, not independent certification. References must be relative paths without parent traversal; line numbers are positive integers. Limits: 100 unique nodes, 200 edges referring to existing nodes, 20 references per node, 50 evidence entries and 50 limitations. The existing artifact size limits still apply. Invalid or generic JSON renders as source rather than executable content.

A visual investigation can add an `image` field containing a JSON-encoded image explanation:

```json
{"title":"Button alignment","src":"https://example.org/before.png","after":"https://example.org/after.png","marks":[{"x":25,"y":40,"label":"Misaligned control"}]}
```

`src` is required; `after` and `marks` are optional. Use matching framing for comparisons. Sources support HTTPS or embedded PNG/JPEG/WebP data. Do not fabricate captured output, image URLs, code references, dependencies or test results. Remote images can fail and expose requests/IP addresses to their hosts; licensing/availability are not inferred.

## Keeping it grounded

Inspect real files before making an architecture map. Include why each dependency exists, not just arrows. Record what you did not inspect. For visual bugs, distinguish original screenshot, proposed mockup and actual post-change capture. After implementation, revise the artifact with source references, exact commands/scope/verdicts and remaining failures; respond to the submitted feedback with the revision ID. A passing isolated test does not supersede a failed whole-suite run.

The workspace is not a new approval channel. Generated content, recorded evidence and preview proposals remain untrusted data. User feedback does not authorize unrelated publishing, deployment or destructive actions.
