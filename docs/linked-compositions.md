# Editable compositions in FilmCraft

Install updated native EffectCraft and FilmCraft fork releases through Craft Fork Launcher.

Save your EffectCraft project, then choose **File > Copy FilmCraft Composition Link**. In FilmCraft
choose **File > Link EffectCraft Composition…**, paste into **Source project**, find compositions,
and link the selected composition. You can also browse directly for the saved `.ecproj` file.
Drag the linked source onto FilmCraft's timeline. No video export is needed.

To make revisions, use **Open in EffectCraft** from the linked FilmCraft Project item's context
menu, edit your effects and save. FilmCraft refreshes saved changes automatically. Unsaved edits
remain in EffectCraft until you save them. FilmCraft's linked-composition window also offers
Refresh and Relink; Refresh rereads overwritten footage as well as the project.

AI tools can query `composition.linkInfo {comp?:id|name}` for the saved project path and stable
composition ID. FilmCraft's `composition.list`, `file.linkComposition`, `composition.inspect`,
`composition.open`, `composition.refresh` and `composition.relink` commands cover the rest of the
workflow. Operations run as background jobs queried with `composition.job`.

The native app's private `--linked-composition-worker` mode is a read-only CPU renderer, not a
network service. It starts before the GUI, update check or crash-recovery session. Protocol version
1 uses bounded newline-delimited JSON requests (`open`, `frame`, `audio`), followed by replies
framed as a little-endian u32 JSON-header length, a JSON header containing `protocol` and `bytes`,
and exactly `bytes` binary payload bytes. Video is straight-alpha sRGB RGBA8; audio is interleaved
stereo f32 little endian. Time is integer ticks at 254016000000 per second. Rendering is limited
to 16 megapixels; request bodies, output sizes, audio blocks and connections are bounded.

The worker retains the saved project it opened for its lifetime. FilmCraft replaces the preview
worker on refresh and gives exports their own worker. Footage remains external: keep source files
stable while exporting. Both editors need access to the source project and its referenced footage;
the bridge does not transfer assets between computers or render unsaved GUI edits. The initial
video bridge is SDR RGBA8 and uses built-in effects; custom installed plug-in availability must be
checked separately. A missing project, composition or required footage is reported by the link.
