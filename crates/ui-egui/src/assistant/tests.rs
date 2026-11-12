use super::*;
use crate::control;
use effectcraft_engine::Session;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

#[test]
fn guide_observation_preserves_widget_local_value_changes() {
    let mut app = EffectcraftApp::new(Session::default());
    let ctx = egui::Context::default();
    let mut output = ctx.run_ui(Default::default(), |ui| {
        let mut response = ui.button("Value editor");
        response.mark_changed();
        app.auto.add_response("value.editor", &response, "Value editor");
        assert!(!ui.ctx().read_response(response.id).unwrap().changed(), "the reconstructed response cannot contain the widget-local flag");
        let adapter = Adapter { app: &mut app, ctx: ui.ctx() };
        assert!(adapter.guide_observation("value.editor", ui.ctx()).unwrap().changed);
    });
    output.textures_delta.clear();
}

#[test]
fn how_to_guidance_highlights_a_live_panel_without_editing_the_project() {
    use egui_kittest::{Harness, kittest::Queryable};
    let mut app = EffectcraftApp::new(Session::default());
    app.show_panel(crate::dock::PanelKind::AiChat);
    app.session.prefs.ai.attach_frames = false;
    let mut h = Harness::builder().with_size(egui::vec2(1500.0, 1000.0)).build_eframe(move |_| app);
    h.run_steps(4);
    let target = format!("panel.{}", crate::dock::PanelKind::Project.id());
    assert!(h.state().auto.find(&target).is_some());
    let project = h.state().session.project.clone();
    *h.state_mut().assistant.panel.prompt_mut() = "How do I find my footage?".into();
    let ctx = h.ctx.clone();
    let id = target.clone();
    let mut step = 0;
    begin_with(h.state_mut(), &ctx, move |_| {
        step += 1;
        if step == 1 { Ok(json!({"status":"completed","output":[{"type":"function_call","call_id":"guide","name":"highlight_ui","arguments":json!({"targets":[{"id":id,"label":"Find footage in the Project panel"}]}).to_string()}]})) }
        else { Ok(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Use the highlighted Project panel to find footage."}]}]})) }
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    while h.state().assistant.panel.busy() && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!h.state().assistant.panel.busy());
    h.run_steps(3);
    let status = route(h.state_mut(), &ctx, "ai.status").unwrap().unwrap();
    assert_eq!(status["highlights"][0]["id"], target);
    assert_eq!(status["highlights"][0]["visible"], true);
    assert!(h.query_by_label("1. Find footage in the Project panel (control hidden)").is_none());
    assert!(Arc::ptr_eq(&project, &h.state().session.project));
    if let Some(dir) = std::env::var_os("CRAFT_AI_EDITOR_FIXTURES") {
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(std::path::Path::new(&dir).join("effectcraft-guidance.png")).unwrap();
    }
    route(h.state_mut(), &ctx, "ai.clearHighlights").unwrap().unwrap();
    assert_eq!(route(h.state_mut(), &ctx, "ai.status").unwrap().unwrap()["highlights"], json!([]));
}

fn turn(app: &mut EffectcraftApp) {
    let ctx = egui::Context::default();
    let deadline = Instant::now() + Duration::from_secs(8);
    while app.assistant.panel.busy() && Instant::now() < deadline {
        poll(app, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!app.assistant.panel.busy(), "AI turn did not complete");
}

fn message() -> Value {
    json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Created the composition."}]}]})
}
fn create_comp() -> Value {
    json!({"status":"completed","output":[{"type":"function_call","call_id":"edit1","name":"effectcraft",
        "arguments":json!({"method":"engine.execute","params_json":json!({"command":"comp.new","params":{"name":"AI composition","width":128,"height":72}}).to_string()}).to_string()}]})
}

#[test]
fn mock_openai_turn_uses_real_command_dispatch_and_undo() {
    let mut app = EffectcraftApp::new(Session::default());
    *app.assistant.panel.prompt_mut() = "Create a composition".into();
    app.session.prefs.ai.attach_frames = false;
    let mut count = 0;
    begin_with(&mut app, &egui::Context::default(), move |body| {
        count += 1;
        if count == 1 {
            Ok(create_comp())
        } else {
            let output = body["input"].as_array().unwrap().last().unwrap()["output"].as_str().unwrap();
            assert_eq!(serde_json::from_str::<Value>(output).unwrap()["ok"], true);
            Ok(message())
        }
    })
    .unwrap();
    turn(&mut app);
    assert_eq!(app.session.project.item(app.session.active_comp_id().unwrap()).unwrap().name, "AI composition");
    let label = effectcraft_engine::command_specs().iter().find(|spec| spec.id == "comp.new").unwrap().label;
    let activity = app.assistant.panel.transcript().iter().find(|(role, _)| role == "Tool").unwrap();
    assert_eq!(activity.1, format!("{label} — Done"));
    assert!(!activity.1.contains("params_json"));
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.comps().count(), 0);
}

#[test]
fn project_replacement_rejects_an_awaiting_edit() {
    let mut app = EffectcraftApp::new(Session::default());
    *app.assistant.panel.prompt_mut() = "Do work".into();
    app.session.prefs.ai.attach_frames = false;
    let (start, wait) = mpsc::channel();
    begin_with(&mut app, &egui::Context::default(), move |_| {
        wait.recv_timeout(Duration::from_secs(2)).unwrap();
        Ok(create_comp())
    })
    .unwrap();
    app.session.execute("file.newProject", json!({})).unwrap();
    start.send(()).unwrap();
    turn(&mut app);
    assert_eq!(app.session.project.comps().count(), 0);
    assert!(app.assistant.panel.context_items() == 0);
}

#[test]
fn stop_rejects_queued_actions() {
    let mut app = EffectcraftApp::new(Session::default());
    *app.assistant.panel.prompt_mut() = "Do work".into();
    app.session.prefs.ai.attach_frames = false;
    begin_with(&mut app, &egui::Context::default(), |_| Ok(create_comp())).unwrap();
    app.assistant.panel.stop();
    turn(&mut app);
    assert!(app.assistant.panel.message().contains("Stopped"));
    assert_eq!(app.session.project.comps().count(), 0);
}

#[test]
fn credentials_and_messages_are_not_in_serialized_editor_data() {
    let mut app = EffectcraftApp::new(Session::default());
    app.assistant.panel.set_session_key("sk-private-test-credential".into()).unwrap();
    // Queue a private conversation through the shared plugin; no serializer can observe it.
    *app.assistant.panel.prompt_mut() = "private conversation".into();
    begin_with(&mut app, &egui::Context::default(), |_| {
        Ok(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"private conversation"}]}]}))
    })
    .unwrap();
    turn(&mut app);
    *app.assistant.panel.prompt_mut() = "private draft".into();
    let (req, _) = crate::ControlRequest::new("ui.inspect", json!({}));
    let control::Outcome::Done(inspected) = control::handle(&mut app, &egui::Context::default(), &req, 0.0) else { panic!("inspect must complete") };
    let project = serde_json::to_string(&app.session.project).unwrap();
    let prefs = app.session.prefs.to_json();
    for secret in ["sk-private-test-credential", "private draft", "private conversation"] {
        assert!(!inspected.to_string().contains(secret));
        assert!(!project.contains(secret));
        assert!(!prefs.contains(secret));
    }
    let status = route(&mut app, &egui::Context::default(), "ai.status").unwrap().unwrap().to_string();
    assert!(!status.contains("sk-private-test-credential"));
}

#[test]
fn hostile_tool_arguments_and_self_invocation_are_rejected() {
    for (name, args) in [
        ("shell", json!({})),
        ("effectcraft", json!({"method":"app.quit","params_json":"{}"})),
        ("effectcraft", json!({"method":"ui.type","params_json":"[]"})),
        ("effectcraft", json!({"method":"engine.execute","params_json":"{bad"})),
        ("effectcraft", json!({"method":"engine.execute","params_json":"{\"command\":\"ai.send\"}"})),
        ("effectcraft", json!({"method":"ui.focus","params_json":"{\"id\":\"ai.key\"}"})),
        ("effectcraft", json!({"method":"engine.execute","params_json":"{\"command\":\"prefs.set\",\"params\":{\"key\":\"ai.model\",\"value\":\"wrong\"}}"})),
        ("video_frame", json!({"monitor":"Desktop"})),
        ("video_frame", json!({"monitor":"Composition","path":"secret"})),
    ] {
        assert!(tools::parse(name, &args).is_err(), "accepted {name} {args}");
    }
    assert!(tools::parse("effectcraft", &json!({"method":"engine.commands","params_json":"{}"})).is_ok());
}

#[test]
fn actual_composition_frame_is_attached_without_seeking() {
    let mut app = EffectcraftApp::new(Session::default());
    app.session.execute("comp.new", json!({"width":128,"height":72,"frameRate":24})).unwrap();
    app.session.execute("time.set", json!({"frame":12})).unwrap();
    let time = app.session.time();
    let mut snapshot = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = tools::frame(&app, "Composition", &mut snapshot).unwrap() {
            break frame;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    let meta: Value = serde_json::from_str(frame[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(meta["width"], 128);
    assert_eq!(meta["height"], 72);
    assert_eq!(meta["ticks"], time.0);
    assert!(frame[1]["image_url"].as_str().unwrap().starts_with("data:image/png;base64,iVBOR"));
    assert_eq!(app.session.time(), time);
}

#[test]
fn narrow_panel_keeps_composer_inside_its_bounds() {
    use egui_kittest::Harness;
    let app = EffectcraftApp::new(Session::default());
    let rect = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(220.0, 490.0));
    let mut h = Harness::builder().with_size(rect.size()).build_ui_state(
        |ui, app| {
            // Measure the accepted layout, not a discarded measurement pass retained by the registry.
            app.auto.elements.clear();
            show(app, ui, ui.max_rect());
        },
        app,
    );
    h.run_steps(5);
    for id in ["ai.prompt", "ai.send", "ai.clear", "ai.attach", "ai.settings"] {
        let r = h.state().auto.find(id).unwrap().rect;
        assert!(r[0] >= 0.0 && r[1] >= 0.0 && r[0] + r[2] <= rect.width() + 1.0 && r[1] + r[3] <= rect.height() + 1.0, "{id}: {r:?}");
    }
}

#[test]
fn preferences_persist_and_recover_without_plaintext_credentials() {
    let mut prefs = effectcraft_engine::prefs::Prefs::default();
    prefs.set("ai.model", json!("gpt-4.1")).unwrap();
    prefs.set("ai.attachFrames", json!(false)).unwrap();
    prefs.set("ai.rememberKey", json!(false)).unwrap();
    let back = effectcraft_engine::prefs::Prefs::from_json(&prefs.to_json());
    assert_eq!(back.ai, prefs.ai);
    assert!(prefs.set("ai.apiKey", json!("sk-secret")).is_err());
    assert!(prefs.set("ai.model", json!("bad\nmodel")).is_err());
    let recovered = effectcraft_engine::prefs::Prefs::from_json(r#"{"general":{"undoLevels":12},"ai":{"model":null,"apiKey":"sk-secret"}}"#);
    assert_eq!(recovered.general.undo_levels, 12);
    assert_eq!(recovered.ai.model, craft_ai_chat_egui::core::DEFAULT_MODEL);
    assert!(!recovered.to_json().contains("sk-secret"));
}

#[test]
fn footage_frame_uses_its_own_time_without_changing_the_editor() {
    use effectcraft_engine::project::{Footage, FootageKind, ItemId, ItemKind};
    use effectcraft_engine::render::{FootageSource, Image};
    use effectcraft_engine::time::{FrameRate, Tick};
    struct Source(Arc<std::sync::Mutex<Vec<Tick>>>);
    impl FootageSource for Source {
        fn frame(&self, _: ItemId, _: &Footage, time: Tick) -> Option<Arc<Image>> {
            self.0.lock().unwrap().push(time);
            Some(Arc::new(Image::filled(64, 32, [0.8, 0.2, 0.0, 1.0])))
        }
    }
    let mut app = EffectcraftApp::new(Session::default());
    let times = Arc::new(std::sync::Mutex::new(vec![]));
    app.session.footage = Arc::new(Source(times.clone()));
    let footage = Footage {
        width: 64,
        height: 32,
        has_video: true,
        kind: FootageKind::Video,
        duration: Tick::from_seconds_f64(4.0),
        frame_rate: FrameRate::FPS_24,
        ..Default::default()
    };
    let item = Arc::make_mut(&mut app.session.project).add_item("Synthetic shot", Default::default(), None, ItemKind::Footage(footage));
    app.session.execute("footage.open", json!({"item":item.0})).unwrap();
    app.session.execute("footage.setTime", json!({"frame":24})).unwrap();
    let before = app.session.state.clone();
    let mut pending = None;
    let deadline = Instant::now() + Duration::from_secs(5);
    let frame = loop {
        if let Some(frame) = tools::frame(&app, "Footage", &mut pending).unwrap() {
            break frame;
        }
        assert!(Instant::now() < deadline);
        std::thread::sleep(Duration::from_millis(2));
    };
    let metadata: Value = serde_json::from_str(frame[0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["frame"], 24);
    assert_eq!(metadata["ticks"], Tick::from_seconds_f64(1.0).0);
    assert_eq!(*times.lock().unwrap(), vec![Tick::from_seconds_f64(1.0)]);
    assert_eq!(serde_json::to_value(&app.session.state).unwrap(), serde_json::to_value(before).unwrap());
}

#[test]
fn ai_panel_docks_floats_reopens_and_saves_its_workspace() {
    use crate::dock::{self, PanelKind, WorkspaceExt as _};
    use craft_ui::docking::Action;
    let mut app = EffectcraftApp::new(Session::default());
    let panel = PanelKind::AiChat;
    route(&mut app, &egui::Context::default(), "ai.chat").unwrap().unwrap();
    assert!(app.ui.dock.is_open(panel));
    app.ui.dock.apply(Action::Float { panel, rect: [20.0, 20.0, 400.0, 500.0] }, dock::permissions).unwrap();
    assert!(app.ui.dock.is_floating(panel));
    app.ui.dock.apply(Action::Close { panel }, dock::permissions).unwrap();
    assert!(!app.ui.dock.is_open(panel));
    app.show_panel(panel);
    assert!(app.ui.dock.is_floating(panel));
    let config = effectcraft_engine::config::MemoryConfig::default();
    app.workspace_store.save_now(Some(&config), &app.ui).unwrap();
    let mut restored = crate::state::UiState::default();
    let _store = crate::workspace_state::WorkspaceStore::load(Some(&config), &mut restored);
    assert!(restored.dock.is_floating(panel));
    assert_eq!(restored.dock, app.ui.dock);
}

#[test]
fn input_tool_is_dispatched_once_and_waits_for_the_accepted_pass() {
    let mut app = EffectcraftApp::new(Session::default());
    let ctx = egui::Context::default();
    app.session.prefs.ai.attach_frames = false;
    *app.assistant.panel.prompt_mut() = "Step once".into();
    let mut n = 0;
    begin_with(&mut app,&ctx,move |_|{n+=1;Ok(if n==1 {json!({"status":"completed","output":[{"type":"function_call","call_id":"input","name":"effectcraft","arguments":json!({"method":"ui.key","params_json":"{\"key\":\"ArrowRight\"}"}).to_string()}]})}else{json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Stepped"}]}]})})}).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.synthetic.is_empty() && Instant::now() < deadline {
        poll(&mut app, &ctx);
        std::thread::sleep(Duration::from_millis(2));
    }
    let count = app.synthetic.len();
    assert!(count > 0);
    assert!(app.want_paint);
    assert_eq!(app.input_waiters.len(), 1);
    poll(&mut app, &ctx);
    assert_eq!(app.synthetic.len(), count);
    assert_eq!(app.input_waiters.len(), 1);
    for reply in app.input_waiters.drain(..) {
        reply.send(json!({"ok":true,"result":null})).unwrap();
    }
    turn(&mut app);
    assert_eq!(app.synthetic.len(), count);
    assert!(app.assistant.panel.message().contains("Ready"));
}

#[test]
fn forget_key_removes_the_password_fields_undo_history() {
    use egui_kittest::{Harness, kittest::Queryable};
    let app = EffectcraftApp::new(Session::default());
    let mut h = Harness::builder().with_size(egui::vec2(500.0, 500.0)).build_ui_state(|ui, app| credentials_ui(app, ui, false), app);
    h.run_steps(4);
    let ctx = h.ctx.clone();
    let id = h.state().auto.find("ai.key").unwrap().response_id.unwrap();
    ctx.memory_mut(|memory| memory.request_focus(id));
    h.input_mut().events.push(egui::Event::Text("sk-private-test-credential".into()));
    h.run_steps(3);
    assert!(h.state().assistant.panel.has_key());
    h.get_by_label("Forget key").click();
    h.run_steps(3);
    assert!(!h.state().assistant.panel.has_key());
    ctx.memory_mut(|memory| memory.request_focus(id));
    h.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers { command: true, ..Default::default() }));
    h.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Z,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers { command: true, ..Default::default() },
    });
    h.run_steps(3);
    assert!(!h.state().assistant.panel.has_key(), "Undo must not restore a forgotten API key");
}

#[test]
fn activity_names_use_the_editors_command_labels() {
    let mut app = EffectcraftApp::new(Session::default());
    let ctx = egui::Context::default();
    let host = Adapter { app: &mut app, ctx: &ctx };
    for command in effectcraft_engine::command_specs().iter().take(8) {
        let call = ToolCall::new("effectcraft", json!({"method":"engine.execute","params_json":json!({"command":command.id,"params":{}}).to_string()}));
        assert_eq!(host.tool_label(&call), command.label);
    }
}

#[test]
fn walkthrough_follows_real_menu_clicks_without_editing_the_project() {
    use egui_kittest::{Harness, kittest::Queryable};
    let mut app = EffectcraftApp::new(Session::default());
    app.show_panel(crate::dock::PanelKind::AiChat);
    app.session.prefs.ai.attach_frames = false;
    let before = app.session.project.clone();
    let mut h = Harness::builder().with_size(egui::vec2(1500.0, 1000.0)).build_eframe(move |_| app);
    h.run_steps(4);
    let ctx = h.ctx.clone();
    *h.state_mut().assistant.panel.prompt_mut() = "Guide me through the File and Edit menus".into();
    let mut turn = 0;
    begin_with(h.state_mut(),&ctx,move |_|{
        let current=turn;turn+=1;
        if current<2 {
            let name=if current==0 {"File"} else {"Edit"};
            Ok(json!({"status":"completed","output":[{"type":"function_call","call_id":format!("step{current}"),"name":"guide_ui","arguments":json!({"id":format!("menu.{name}"),"label":format!("Open the {name} menu"),"instruction":format!("Click **{name}** in the menu bar."),"completion":"activate","watch_id":"","state_pointer":"","comparison":"equal","expected_json":""}).to_string()}]}))
        }else{Ok(json!({"status":"completed","output":[{"type":"message","role":"assistant","content":[{"type":"output_text","text":"Both menus are ready."}]}]}))}
    }).unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while route(h.state_mut(), &ctx, "ai.status").unwrap().unwrap()["walkthrough"]["step"] != 1 && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(4);
    assert!(Arc::ptr_eq(&before, &h.state().session.project));
    h.query_by_role_and_label(egui::accesskit::Role::Button, "File").unwrap().click();
    h.run_steps(3);
    while route(h.state_mut(), &ctx, "ai.status").unwrap().unwrap()["walkthrough"]["step"] != 2 && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    h.run_steps(3);
    let status = route(h.state_mut(), &ctx, "ai.status").unwrap().unwrap();
    assert_eq!(status["highlights"][0]["id"], "menu.Edit");
    assert_eq!(status["highlights"][0]["number"], 2);
    if let Some(dir) = std::env::var_os("CRAFT_AI_EDITOR_FIXTURES") {
        std::fs::create_dir_all(&dir).unwrap();
        h.render().unwrap().save(std::path::Path::new(&dir).join("effectcraft-walkthrough.png")).unwrap();
    }
    h.query_by_role_and_label(egui::accesskit::Role::Button, "Edit").unwrap().click();
    h.run_steps(3);
    while h.state().assistant.panel.busy() && Instant::now() < deadline {
        h.run_steps(1);
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(!h.state().assistant.panel.busy());
    assert!(Arc::ptr_eq(&before, &h.state().session.project));
    assert_eq!(route(h.state_mut(), &ctx, "ai.status").unwrap().unwrap()["highlights"], json!([]));
}
