use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};

use serde::Deserialize;
use serde_json::{Value, json};

use crate::{ControlRequest, EffectcraftApp};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EditorArgs {
    method: String,
    params_json: String,
}

pub(super) enum Tool {
    Editor(ControlRequest),
    Frame { monitor: String, snapshot: Option<Snapshot> },
}

pub(super) struct Snapshot {
    revision: u64,
    receive: mpsc::Receiver<Result<Value, String>>,
}

pub(super) fn parse(name: &str, args: &Value) -> Result<(Tool, mpsc::Receiver<Value>, String), String> {
    match name {
        "effectcraft" => {
            let args: EditorArgs = serde_json::from_value(args.clone()).map_err(|_| "Expected method and params_json strings")?;
            let supported = super::api::tools();
            let allowed = supported
                .pointer("/0/parameters/properties/method/enum")
                .and_then(Value::as_array)
                .is_some_and(|methods| methods.iter().any(|m| m.as_str() == Some(&args.method)));
            if !allowed {
                return Err("Unsupported EffectCraft method; use the tool's declared methods".into());
            }
            if args.params_json.len() > 64 * 1024 {
                return Err("Editor parameters exceed 64 KiB".into());
            }
            let params: Value = serde_json::from_str(&args.params_json).map_err(|_| "params_json must contain a valid JSON object")?;
            if !params.is_object() {
                return Err("params_json must contain a JSON object".into());
            }
            // Credentials and Send controls belong to the human, never to the model's tool loop.
            if targets_chat(&params, 0) {
                return Err("AI chat controls and preferences are reserved for the user".into());
            }
            let label = format!("{} {}", args.method, args.params_json.chars().take(1024).collect::<String>());
            let (request, receiver) = ControlRequest::new(args.method, params);
            Ok((Tool::Editor(request), receiver, label))
        }
        "video_frame" => {
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct Args {
                monitor: String,
            }
            let args: Args = serde_json::from_value(args.clone()).map_err(|_| "Expected a monitor: Composition or Footage")?;
            if !matches!(args.monitor.as_str(), "Composition" | "Footage") {
                return Err("monitor must be Composition or Footage".into());
            }
            let (_, rx) = mpsc::channel();
            Ok((Tool::Frame { monitor: args.monitor.clone(), snapshot: None }, rx, format!("Video frame: {}", args.monitor)))
        }
        _ => Err("Unknown AI tool".into()),
    }
}

fn targets_chat(value: &Value, depth: usize) -> bool {
    if depth > 32 {
        return true;
    }
    match value {
        Value::Object(object) => object.iter().any(|(key, value)| {
            key == "ai"
                || key.starts_with("ai.")
                || matches!(key.as_str(), "command" | "id" | "key" | "page")
                    && value.as_str().is_some_and(|id| id == "ai" || id.starts_with("ai.") || id.starts_with("settings.ai."))
                || targets_chat(value, depth + 1)
        }),
        Value::Array(values) => values.iter().any(|value| targets_chat(value, depth + 1)),
        _ => false,
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
