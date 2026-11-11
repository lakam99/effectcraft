use crate::{ControlRequest, EffectcraftApp, control};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
pub(super) enum Tool {
    Editor(ControlRequest),
    Frame { monitor: String, snapshot: Option<Snapshot> },
}

pub(super) struct Snapshot {
    revision: u64,
    receive: mpsc::Receiver<Result<Value, String>>,
}

pub(crate) struct Pending {
    tool: Tool,
    control_reply: std::sync::mpsc::Receiver<Value>,
}
pub(super) fn parse(name: &str, args: &Value) -> Result<(Tool, std::sync::mpsc::Receiver<Value>, String), String> {
    let (tool, rx) = match super::editor_tools().parse(name, args)? {
        craft_ai_chat_egui::core::editor::EditorCall::Command { method, params } => {
            let (req, rx) = ControlRequest::new(method, params);
            (Tool::Editor(req), rx)
        }
        craft_ai_chat_egui::core::editor::EditorCall::View(monitor) => {
            let (_, rx) = std::sync::mpsc::channel();
            (Tool::Frame { monitor, snapshot: None }, rx)
        }
    };
    Ok((tool, rx, name.into()))
}
pub(super) fn start(call: &craft_ai_chat_egui::ToolCall) -> Result<Pending, String> {
    let (tool, control_reply, _) = parse(&call.name, &call.arguments)?;
    Ok(Pending { tool, control_reply })
}
pub(super) fn poll(app: &mut EffectcraftApp, ctx: &egui::Context, pending: &mut Pending) -> Result<Option<Value>, String> {
    match &mut pending.tool {
        Tool::Frame { monitor, snapshot } => frame(app, monitor, snapshot),
        Tool::Editor(req) => match pending.control_reply.try_recv() {
            Ok(value) => Ok(Some(Value::String(value.to_string()))),
            Err(std::sync::mpsc::TryRecvError::Disconnected) => Err("Editor input channel closed".into()),
            Err(std::sync::mpsc::TryRecvError::Empty) if req.method.is_empty() => Ok(None),
            Err(std::sync::mpsc::TryRecvError::Empty) => match control::handle(app, ctx, req, app.now) {
                control::Outcome::Done(value) => Ok(Some(Value::String(value.to_string()))),
                control::Outcome::Retry(_) => Ok(None),
                control::Outcome::AfterInput => {
                    app.input_waiters.push(req.reply.clone());
                    app.want_paint = true;
                    req.method.clear();
                    Ok(None)
                }
                control::Outcome::AwaitJobs(waiter) => {
                    app.job_waiters.push((waiter, req.reply.clone()));
                    req.method.clear();
                    Ok(None)
                }
                control::Outcome::AwaitScript(host) => {
                    app.script_waiters.push((host, req.reply.clone()));
                    req.method.clear();
                    Ok(None)
                }
                control::Outcome::Screenshot { .. } => Err("Use active-view capture to inspect media".into()),
            },
        },
    }
}

struct Rendering(Arc<AtomicBool>);
impl Drop for Rendering {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

/// Pin the current source and exact time. Rendering and PNG encoding never run on the UI thread.
fn snapshot(app: &EffectcraftApp, monitor: &str) -> Result<Snapshot, String> {
    let (job, metadata): (effectcraft_engine::ThumbnailJob, Value) = if monitor == "Composition" {
        let id = app.session.active_comp_id().ok_or("No composition is open")?;
        let comp = app.session.project.comp(id).ok_or("The composition is unavailable")?;
        dimensions(comp.width, comp.height)?;
        let frame = comp.frame_rate.frame_at(app.session.time());
        let time = comp.frame_rate.tick_of(frame);
        let job = app.session.thumbnail_job(id, time, 1280).ok_or("The composition has no picture")?;
        (job, json!({"monitor":monitor,"item":id.0,"ticks":time.0,"frame":frame}))
    } else {
        let view = app.session.state.footage_panel.as_ref().ok_or("No video is open in the Footage panel")?;
        let Some(effectcraft_engine::project::ItemKind::Footage(footage)) = app.session.project.item(view.item).map(|i| &i.kind) else {
            return Err("The Footage panel has no video".into());
        };
        if !footage.has_video {
            return Err("The Footage item has no video".into());
        }
        dimensions(footage.width, footage.height)?;
        let frame = footage.frame_rate.frame_at(view.time);
        let time = footage.frame_rate.tick_of(frame);
        let (source, footage, id) = (app.session.footage.clone(), footage.clone(), view.item);
        let job = Box::new(move || {
            let image = effectcraft_engine::render::cineon_frame(source.frame(id, &footage, time)?, &footage);
            // Downsample before conversion: a decoded 8K frame must not allocate another full RGBA buffer.
            if dimensions(image.width, image.height).is_err() {
                return None;
            }
            let step = image.width.max(image.height).div_ceil(1280).max(1);
            let (w, h) = (image.width.div_ceil(step), image.height.div_ceil(step));
            let mut rgba = Vec::with_capacity((w as usize).saturating_mul(h as usize).saturating_mul(4));
            for y in 0..h {
                for x in 0..w {
                    let index = (y.saturating_mul(step) as usize).checked_mul(image.width as usize)?.checked_add(x.saturating_mul(step) as usize)?;
                    let pixel = image.data.get(index)?;
                    let mut bytes = pixel.map(|v| if v.is_finite() { (v.clamp(0.0, 1.0) * 255.0).round() as u8 } else { 0 });
                    // The viewer shows premultiplied colour over black.
                    bytes[3] = 255;
                    rgba.extend(bytes);
                }
            }
            Some((w, h, rgba))
        });
        (job, json!({"monitor":monitor,"item":id.0,"ticks":time.0,"frame":frame}))
    };
    let busy = app.assistant.image_busy.clone();
    if busy.swap(true, Ordering::AcqRel) {
        return Err("A previous AI frame is still rendering. Try again when it finishes.".into());
    }
    let guard = Rendering(busy);
    let (sender, receive) = mpsc::channel();
    std::thread::Builder::new()
        .name("effectcraft-ai-frame".into())
        .spawn(move || {
            let rendering = guard;
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let (w, h, rgba) = job().ok_or("Cannot render this video frame")?;
                encode(w, h, rgba, metadata)
            }))
            .unwrap_or_else(|_| Err("Video frame rendering failed. Your project is still open.".into()));
            drop(rendering);
            let _ = sender.send(result);
        })
        .map_err(|_| "Cannot start the video frame worker")?;
    Ok(Snapshot { revision: app.session.revision, receive })
}

fn dimensions(w: u32, h: u32) -> Result<(), String> {
    if w == 0 || h == 0 || w > 30_000 || h > 30_000 { Err("The video has invalid dimensions".into()) } else { Ok(()) }
}

fn encode(w: u32, h: u32, rgba: Vec<u8>, mut metadata: Value) -> Result<Value, String> {
    if w == 0 || h == 0 || w > 1280 || h > 1280 || rgba.len() != (w as usize).saturating_mul(h as usize).saturating_mul(4) {
        return Err("The rendered frame exceeds the AI image limit or has an invalid pixel buffer".into());
    }
    let image = image::RgbaImage::from_raw(w, h, rgba).ok_or("Invalid video pixel buffer")?;
    let mut png = std::io::Cursor::new(Vec::new());
    image.write_to(&mut png, image::ImageFormat::Png).map_err(|_| "Cannot encode the video frame")?;
    let png = png.into_inner();
    if png.len() > 4 * 1024 * 1024 {
        return Err("Video PNG exceeds the AI image size limit".into());
    }
    metadata["width"] = json!(w);
    metadata["height"] = json!(h);
    Ok(json!([
        {"type":"input_text","text":metadata.to_string()},
        {"type":"input_image","image_url":format!("data:image/png;base64,{}", crate::control::base64(&png)),"detail":"auto"}
    ]))
}

pub(super) fn frame(app: &EffectcraftApp, monitor: &str, pending: &mut Option<Snapshot>) -> Result<Option<Value>, String> {
    if pending.is_none() {
        *pending = Some(snapshot(app, monitor)?);
    }
    let frame = pending.as_ref().ok_or("Cannot prepare the video frame")?;
    if frame.revision != app.session.revision {
        return Err("The project changed while the frame was rendering. Request another frame.".into());
    }
    match frame.receive.try_recv() {
        Ok(result) => result.map(Some),
        Err(mpsc::TryRecvError::Empty) => Ok(None),
        Err(mpsc::TryRecvError::Disconnected) => Err("The video frame worker stopped".into()),
    }
}
