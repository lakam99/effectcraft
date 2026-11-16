//! Read-only linked-composition renderer. No window, sockets, project writes or desktop settings.
//! Requests are bounded JSON lines. Replies are a little-endian u32 JSON length, JSON metadata,
//! then exactly `bytes` raw payload bytes (straight sRGB RGBA8 or interleaved stereo f32 LE).

use effectcraft_engine::{
    Session,
    project::{ItemId, ItemKind},
    time::Tick,
};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};

const MAX_REQUEST: u64 = 65_536;
const MAX_PIXELS: u64 = 16_777_216;

struct ReadOnlyFiles;
impl effectcraft_engine::Services for ReadOnlyFiles {
    fn read_file(&self, path: &str) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?.take(128 * 1024 * 1024 + 1).read_to_end(&mut bytes)?;
        if bytes.len() > 128 * 1024 * 1024 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "linked project exceeds 128 MiB"));
        }
        Ok(bytes)
    }
    fn write_file(&self, _: &str, _: &[u8]) -> std::io::Result<()> {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the linked composition renderer is read-only"))
    }
}

pub fn run(input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    let mut input = input;
    let mut session = None;
    loop {
        let mut line = String::new();
        let n = input.by_ref().take(MAX_REQUEST + 1).read_line(&mut line).map_err(|e| e.to_string())?;
        if n == 0 {
            return Ok(());
        }
        if n as u64 > MAX_REQUEST || !line.ends_with('\n') {
            return Err("composition request exceeds 64 KiB".into());
        }
        let reply = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let request: Value = serde_json::from_str(&line).map_err(|e| e.to_string())?;
            handle(&mut session, &request)
        }))
        .unwrap_or_else(|_| Err("EffectCraft could not render this composition; the editor's project is safe".into()));
        let (mut meta, data) = match reply {
            Ok(reply) => reply,
            Err(error) => (json!({"error": error}), vec![]),
        };
        meta["protocol"] = json!(1);
        meta["bytes"] = json!(data.len());
        let header = serde_json::to_vec(&meta).map_err(|e| e.to_string())?;
        let len = u32::try_from(header.len()).map_err(|_| "composition response is too large")?;
        output
            .write_all(&len.to_le_bytes())
            .and_then(|()| output.write_all(&header))
            .and_then(|()| output.write_all(&data))
            .and_then(|()| output.flush())
            .map_err(|e| e.to_string())?;
    }
}

fn integer(p: &Value, key: &str) -> Result<i64, String> {
    p.get(key).and_then(Value::as_i64).ok_or_else(|| format!("missing or invalid {key}"))
}

fn handle(slot: &mut Option<Session>, p: &Value) -> Result<(Value, Vec<u8>), String> {
    match p.get("method").and_then(Value::as_str) {
        Some("open") => {
            if slot.is_some() {
                return Err("this worker already owns a project snapshot".into());
            }
            let path = p.get("path").and_then(Value::as_str).ok_or("open needs a project path")?;
            let meta = std::fs::metadata(path).map_err(|e| format!("EffectCraft project is unavailable: {e}"))?;
            if meta.len() > 128 * 1024 * 1024 {
                return Err("EffectCraft project exceeds 128 MiB".into());
            }
            let mut s = super::session();
            effectcraft_engine::text::fonts::scan_system();
            s.services = std::sync::Arc::new(ReadOnlyFiles);
            s.execute("file.open", json!({"path": path})).map_err(|e| e.to_string())?;
            s.execute("footage.check", json!({"wait": true})).map_err(|e| e.to_string())?;
            let missing: Vec<_> = s
                .project
                .items
                .values()
                .filter_map(|i| match &i.kind {
                    ItemKind::Footage(f) if f.missing => Some(f.path.clone()),
                    _ => None,
                })
                .collect();
            let comps: Vec<_> = s
                .project
                .items
                .values()
                .filter_map(|i| match &i.kind {
                    ItemKind::Comp(c) => Some(json!({"id": i.id.0, "name": i.name, "width": c.width, "height": c.height,
                    "duration": c.duration.0, "frameRate": c.frame_rate, "pixelAspect": c.pixel_aspect,
                    "missing": effectcraft_engine::project::essential::dependencies(&s.project,i.id).into_iter().filter_map(|id| {
                        match s.project.item(id).map(|i|&i.kind) { Some(ItemKind::Footage(f)) if f.missing => Some(f.path.clone()), _=>None }
                    }).collect::<Vec<_>>(),
                    "audio": effectcraft_engine::render::audio::comp_has_audio(&s.project, i.id)})),
                    _ => None,
                })
                .collect();
            *slot = Some(s);
            Ok((json!({"compositions": comps, "missing": missing, "encoding": "srgb-rgba8-straight"}), vec![]))
        }
        Some(method @ ("frame" | "audio")) => {
            let s = slot.as_ref().ok_or("open a project first")?;
            let cid = p.get("composition").and_then(Value::as_u64).map(ItemId).ok_or("invalid composition id")?;
            let c = s.project.comp(cid).ok_or("the linked composition no longer exists; choose another composition")?;
            let time = integer(p, "time")?;
            if !(Tick::MIN.0..=Tick::MAX.0).contains(&time) {
                return Err("time is out of range".into());
            }
            if method == "frame" {
                if u64::from(c.width) * u64::from(c.height) > MAX_PIXELS || c.width == 0 || c.height == 0 || c.width > 16_384 || c.height > 16_384 {
                    return Err("linked compositions support up to 16 megapixels per frame".into());
                }
                let side = p.get("maxSide").and_then(Value::as_u64).filter(|n| *n <= 16_384).ok_or("invalid maxSide")? as u32;
                let (w, h, rgba) = s.render_rgba8_alpha(cid, Tick(time), side, true).map_err(|e| e.to_string())?;
                Ok((json!({"width": w, "height": h, "encoding": "srgb-rgba8-straight"}), rgba))
            } else {
                let frames = p.get("frames").and_then(Value::as_u64).filter(|n| *n <= 384_000).ok_or("audio requests support up to 384000 frames")? as usize;
                let rate = p.get("rate").and_then(Value::as_u64).filter(|n| (8000..=192_000).contains(n)).ok_or("audio rate must be 8000..192000")? as u32;
                let audio = effectcraft_engine::render::audio::mix_comp(&s.project, &*s.footage, s.expr.as_deref(), cid, Tick(time), frames, rate);
                let bytes = audio.into_iter().flat_map(|x| if x.is_finite() { x } else { 0.0 }.to_le_bytes()).collect();
                Ok((json!({"frames": frames, "channels": 2, "rate": rate, "encoding": "f32le-interleaved"}), bytes))
            }
        }
        _ => Err("unknown linked-composition method".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_hostile_requests_without_panicking() {
        let mut slot = None;
        for p in [json!({}), json!({"method":"frame", "composition": -1}), json!({"method":"open", "path": 1})] {
            assert!(handle(&mut slot, &p).is_err());
        }
        let mut out = vec![];
        run(std::io::Cursor::new(b"{bad}\n{}\n"), &mut out).unwrap();
        assert!(!out.is_empty());
        assert!(run(std::io::Cursor::new(vec![b'a'; 65537]), &mut vec![]).is_err());
    }

    #[test]
    fn linked_worker_renders_effects_alpha_audio_and_pins_saved_project() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("linked.ecproj");
        let mut s = super::super::session();
        let comp = s.execute("comp.new", json!({"name":"Overlay","width":32,"height":24,"duration":1,"frameRate":24})).unwrap()["comp"].as_u64().unwrap();
        let layer = s.execute("layer.newSolid", json!({"color":"#ff0000","width":8,"height":8})).unwrap()["layer"].as_u64().unwrap();
        s.execute("effect.apply", json!({"effect":"ec.audio.tone","layers":[layer]})).unwrap();
        s.execute("file.saveAs", json!({"path":path})).unwrap();
        let saved = std::fs::read(&path).unwrap();
        let mut worker = None;
        let (catalog, _) = handle(&mut worker, &json!({"method":"open","path":path})).unwrap();
        assert_eq!(catalog["compositions"][0]["id"], comp);
        assert_eq!(catalog["compositions"][0]["audio"], true);
        let request = json!({"method":"frame","composition":comp,"time":0,"maxSide":0});
        let (meta, pixels) = handle(&mut worker, &request).unwrap();
        assert_eq!((meta["width"].as_u64(), meta["height"].as_u64()), (Some(32), Some(24)));
        assert_eq!(pixels, s.render_rgba8_alpha(ItemId(comp), Tick::ZERO, 0, true).unwrap().2);
        assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[3] == 0), "transparent regions must survive linking");
        assert!(pixels.as_chunks::<4>().0.iter().any(|p| p[0] > 200 && p[3] > 200));
        let (_, audio) = handle(&mut worker, &json!({"method":"audio","composition":comp,"time":0,"frames":1024,"rate":48000})).unwrap();
        assert_eq!(audio.len(), 1024 * 2 * 4);
        assert!(audio.as_chunks::<4>().0.iter().any(|word| f32::from_le_bytes(*word).abs() > 0.01));
        assert_eq!(std::fs::read(&path).unwrap(), saved, "the private worker must never save the project");
        s.execute("layer.newSolid", json!({"color":"#0000ff"})).unwrap();
        s.execute("file.save", json!({})).unwrap();
        assert_eq!(handle(&mut worker, &request).unwrap().1, pixels, "an export worker keeps its project snapshot");
        let mut refreshed = None;
        handle(&mut refreshed, &json!({"method":"open","path":path})).unwrap();
        assert_ne!(handle(&mut refreshed, &request).unwrap().1, pixels, "a new worker reads saved effects/edits");
        for bad in [
            json!({"method":"frame","composition":comp,"time":i64::MAX,"maxSide":0}),
            json!({"method":"audio","composition":comp,"time":0,"frames":u64::MAX,"rate":48000}),
            json!({"method":"audio","composition":comp,"time":0,"frames":1,"rate":0}),
        ] {
            assert!(handle(&mut refreshed, &bad).is_err());
        }
    }
}
