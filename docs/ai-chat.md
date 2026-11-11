# OpenAI editor assistant

This fork adds **Window → AI Chat**, a dedicated dock panel. Drag its tab to split, tab, float or close it like any other panel. Saved workspaces retain its placement. Closing the panel hides it; **Stop** ends the current assistant turn. Open the panel again through Window.

In **Edit → Preferences → AI Chat** (macOS: **EffectCraft → Settings → AI Chat**), choose the OpenAI model, whether to attach frames automatically, and whether to remember the API key securely. These choices live in the editor's normal `prefs.json`, outside project files. The panel's **Preferences…** button opens that same page.

Enter your OpenAI API key in the panel or settings page. **Save key securely** stores it separately in Windows Credential Manager, macOS Keychain or Linux Secret Service. With secure saving enabled, Send also saves the entered key. Opening AI Chat reloads it after a restart. **Forget key** clears the current field and its undo history and deletes the stored credential. Turn off secure saving for a session-only key; this also removes a previously saved credential. A locked or unavailable credential service reports an error and never falls back to a plaintext file. Each editor configuration directory has its own credential entry, separate from FilmCraft's.

The native app sends messages directly to `https://api.openai.com/v1/responses` with your key. A compatible model must support Responses, image input and function tools. The default matches the FilmCraft fork (`gpt-6.1-sol`); the model ID is editable. Your OpenAI account pays for API usage. Requests use `store: false`; messages and tool history remain in app memory and are not saved in projects or preferences. Consult [OpenAI's data controls](https://developers.openai.com/api/docs/guides/your-data) for provider retention policies.

## Images and editor tools

With **Attach Composition and Footage frames** enabled, each message includes the current composition and the source opened in the Footage panel, when available. The assistant can request another frame during its turn. Frames are rendered in background workers, bounded to 1280 pixels on either axis, with item ID, frame index and exact integer tick time. Capture pins the requested time; it does not seek, change selection or capture the desktop, key field or chat. These are still images; the assistant does not hear audio or watch live playback. Composition images use the project's rendered picture rather than the viewer's grids, selection overlays, magnification or exposure.

The `effectcraft` function uses the editor's existing dispatcher:

- `engine.commands` and `ui.menu.list` discover commands, parameters and enablement.
- `engine.execute` runs existing composition, layer, mask, effect, property, keyframe, expression, scripting, import/export, render and history commands. Scripts retain the editor's existing permission settings.
- `project.summary`, `comp.info`, `layer.tree` and `editor.state` are useful engine query commands.
- UI tools inspect widgets, click, drag, type, scroll, navigate viewers and timelines, control playback and arrange panels.
- `video_frame` returns an in-memory PNG of the current Composition or Footage frame.

Completed edits use normal Undo. Review the assistant's action cards and the resulting picture. **Stop** prevents further tools; it does not undo completed edits or terminate a render job already started. A project replacement cancels the turn before any delayed tool can edit the new project. A new project or model starts fresh model context. **New chat** discards in-memory conversation once the turn has stopped.

The assistant cannot invoke its own Send or credential commands, configure its own preferences, run a separate shell tool or force the app to quit. The loop permits at most 32 tool calls per turn, caps request/reply sizes, and times out unresponsive tools. Use `wait: false` for long rendering or analysis commands and inspect their jobs afterward. An in-flight OpenAI request can take up to 120 seconds to finish after Stop.

AI Chat is desktop-only; the browser build keeps the panel with an explanatory note. Windows credential persistence is checked on Windows. macOS and Linux backends require their respective operating systems for acceptance testing.

## Fork updates

This fork's update checker uses [lakam99/effectcraft releases](https://github.com/lakam99/effectcraft/releases), separately from the upstream application's feed. Unsigned portable Windows releases offer a manual download; they do not bypass the updater's signature checks.

To upgrade a portable copy, close EffectCraft, replace the application files in its existing folder and keep `EffectCraftData`. Moving the preferences profile to a different directory gives it a separate credential entry; enter the key again there.

The API shape follows the official OpenAI [function-calling guide](https://developers.openai.com/api/docs/guides/function-calling).


## Shared Rust plugin

The dock body, Responses client, conversation lifecycle and secure credential store
come from [Craft AI Chat](https://github.com/lakam99/craft-ai-chat), pinned to a
reviewed Git revision in the UI crate. This repository owns only its product
adapter: active-view rendering, command/UI dispatch, preference persistence and
normal dock registration. Existing preference keys and OS credential identities
are preserved.

Other Craft products can integrate the same source plugin through the `Host`
trait, advertising their own tools and document context. See the shared
repository's adapter guide and compiled non-video document example. This requires
rebuilding the host application; it is not a runtime plugin for existing binaries.

## Conversation and Markdown

Use the key-status button at the top of AI Chat to open connection controls.
The composer stays at the bottom: **Enter** sends and **Shift+Enter** adds a line.
During a request, **Stop** replaces Send. **New chat** clears the session.

Messages support Markdown headings, bold and italic text, lists, quotes, links,
fenced code blocks and tables. Use **Copy** on an assistant reply to copy its
original Markdown. Images and HTML are omitted, and local or non-web links are
blocked. Deeply nested Markdown falls back to plain text.

Editor tool calls appear as named action cards with Working, Done, Failed or
Stopped states. Command names come from the editor's command registry. Raw JSON
arguments, payloads and frame encodings stay out of the chat; the model still
receives the full tool results. Errors appear as concise messages on the card.
