# Inline artifact smoke test

The ordinary workflow is still send a task, let the agent work, and answer questions or approvals if they arise. Artifact cards are optional output: the agent should use its judgment and create one only when a visual or structured presentation materially improves the answer. A one-off preview stays in this chat; a project artifact is durable and shared with other chats in the same project.

Paste this into a fresh OpenLeash chat:

> Inspect this project and identify one genuine visual or structured output that would make some useful part of the project easier to understand. Continue the task in ordinary chat if no artifact would materially help. If one would help, choose the right lifecycle yourself: use a temporary inline preview for something useful to explore once in this conversation, or create/revise a saved project artifact if I should be able to revisit or iterate on it across chats. Do not create both copies of the same content. Keep any HTML self-contained, accessible and responsive, use the app theme, include no secrets or external resources, and do not claim it is connected to the real app. Put a concise explanation in chat, not a narration of the source code.
>
> For the smoke test only, do not make unrelated filesystem/tool changes before showing me the result, unless they are necessary for this task. Do not ask me whether I want an artifact; make the best choice and continue.

If the agent creates one, verify:

1. The card appears inline at the artifact-producing call in the conversation—not only in the Artifacts workspace—and the assistant gives a normal concise written response afterward.
2. While it is being generated, the chat shows **Building…**, not raw or partial tool JSON/source. It must not execute until the complete input validates and the tool succeeds.
3. A one-off card is marked as in this chat, renders inline after success, and does not appear in the project Artifacts list. Reopen the chat and confirm its card is still in the transcript.
4. A saved artifact is marked as project-saved, appears inline, and **Open in Artifacts** selects its exact saved version. The persistent artifact remains available to another chat in the same project.
5. If the assistant instead uses an exact `openleash-viz` fence, it renders automatically when the fence is complete; while streaming, the friendly **Working on the interactive preview…** placeholder appears. Ordinary `html` fences and raw HTML remain inert. HTML saved in the workspace still requires clicking Preview there.
6. For a saved artifact, optionally inspect metadata/version history, annotate an exact version, and verify that annotations/state remain private until you explicitly click **Send feedback to agent**. Do not invent feedback or act on an unsent proposal.

Use dummy values only. One-off output is retained in that chat's transcript but is not secure/private storage. Project artifacts under `.openleash/artifacts` are ordinary repository data; never put secrets there. Generated HTML runs JavaScript in an isolated frame, but can still exhaust CPU/memory or hang the webview; Stop may not recover a blocked renderer. Native webview behavior is platform-specific, so do not assume identical bridge behavior across platforms.
