use anyhow::{Context, Result, anyhow};
use egui::{Event, Key, Modifiers as EguiModifiers, PointerButton, Pos2, RawInput, Rect, Vec2};
use hoard_function::{HoardApp, PlatformCommand};
use raw_window_handle::{
    RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle,
};
use smithay_client_toolkit::reexports::{
    calloop::{EventLoop, LoopHandle, PostAction, RegistrationToken},
    calloop_wayland_source::WaylandSource,
    client::{
        Connection, Proxy, QueueHandle,
        globals::registry_queue_init,
        protocol::{
            wl_data_device::WlDataDevice, wl_data_device_manager::DndAction,
            wl_data_source::WlDataSource, wl_keyboard, wl_output, wl_pointer, wl_seat, wl_surface,
        },
    },
};
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    data_device_manager::{
        DataDeviceManagerState, WritePipe,
        data_device::{DataDevice, DataDeviceHandler},
        data_offer::{DataOfferHandler, DragOffer},
        data_source::{DataSourceHandler, DragSource},
    },
    delegate_registry,
    output::{OutputHandler, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        pointer::{
            BTN_BACK, BTN_EXTRA, BTN_FORWARD, BTN_LEFT, BTN_MIDDLE, BTN_RIGHT, BTN_SIDE,
            CursorIcon as SctkCursorIcon, PointerEvent, PointerEventKind, PointerHandler, ThemeSpec,
            ThemedPointer,
        },
    },
    shell::{
        WaylandSurface,
        xdg::{
            XdgShell,
            window::{Window, WindowConfigure, WindowDecorations, WindowHandler},
        },
    },
    shm::{Shm, ShmHandler},
};
use std::{
    fs,
    io::{BufRead, BufReader, Write},
    os::fd::OwnedFd,
    path::PathBuf,
    process::{Command, Stdio},
    ptr::NonNull,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use url::Url;

const URI_LIST: &str = "text/uri-list";
const WAYLAND_WHEEL_LINE_MULTIPLIER: f32 = 1.4;

fn native_trace_enabled() -> bool {
    std::env::var_os("HOARD_NATIVE_TRACE").is_some()
}

struct SeatObjects {
    seat: wl_seat::WlSeat,
    data_device: DataDevice,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    pointer: Option<ThemedPointer>,
}

struct DragSourceState {
    source: DragSource,
    paths: Vec<PathBuf>,
}

pub fn run(start_path: Option<PathBuf>) -> Result<()> {
    if std::env::var_os("WAYLAND_DISPLAY").is_none() {
        return Err(anyhow!(
            "Hoard is native Wayland only; WAYLAND_DISPLAY is not set"
        ));
    }

    let conn = Connection::connect_to_env().context("failed to connect to Wayland compositor")?;
    let (globals, event_queue) =
        registry_queue_init(&conn).context("failed to enumerate Wayland globals")?;
    let qh = event_queue.handle();

    let mut event_loop: EventLoop<NativeWaylandApp> =
        EventLoop::try_new().context("failed to create native Wayland event loop")?;
    let loop_handle = event_loop.handle();
    WaylandSource::new(conn.clone(), event_queue)
        .insert(loop_handle.clone())
        .context("failed to insert Wayland source into event loop")?;

    let compositor =
        CompositorState::bind(&globals, &qh).context("wl_compositor is unavailable")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm is unavailable")?;
    let xdg_shell = XdgShell::bind(&globals, &qh).context("xdg-shell is unavailable")?;
    let data_device_manager = DataDeviceManagerState::bind(&globals, &qh)
        .context("wl_data_device_manager is unavailable")?;

    let surface = compositor.create_surface(&qh);
    let window = xdg_shell.create_window(surface, WindowDecorations::None, &qh);
    window.set_title("Hoard");
    window.set_app_id("org.diedemus.Hoard");
    window.set_min_size(Some((1050, 650)));
    window.commit();

    let instance_desc = wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    };
    let instance = wgpu::Instance::new(instance_desc);

    let display_ptr = NonNull::new(conn.backend().display_ptr() as *mut _)
        .ok_or_else(|| anyhow!("Wayland display pointer was null"))?;
    let surface_ptr = NonNull::new(window.wl_surface().id().as_ptr() as *mut _)
        .ok_or_else(|| anyhow!("Wayland surface pointer was null"))?;

    let raw_display_handle = RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display_ptr));
    let raw_window_handle = RawWindowHandle::Wayland(WaylandWindowHandle::new(surface_ptr));
    let gpu_surface = unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: Some(raw_display_handle),
            raw_window_handle,
        })
    }
    .context("failed to create Vulkan Wayland surface")?;

    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        force_fallback_adapter: false,
        compatible_surface: Some(&gpu_surface),
        apply_limit_buckets: false,
    }))
    .context("no Vulkan adapter can present to this Wayland surface")?;
    let adapter_info = adapter.get_info();
    if adapter_info.backend != wgpu::Backend::Vulkan {
        return Err(anyhow!(
            "non-Vulkan graphics backend selected unexpectedly: {:?}",
            adapter_info.backend
        ));
    }
    if native_trace_enabled() {
        eprintln!(
            "HOARD_NATIVE adapter={:?} name={} backend={:?}",
            adapter_info.device_type, adapter_info.name, adapter_info.backend
        );
    }

    let (device, queue) =
        pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor::default()))
            .context("failed to create Vulkan device")?;

    let caps = gpu_surface.get_capabilities(&adapter);
    let surface_format = caps
        .formats
        .iter()
        .copied()
        .find(|f| {
            matches!(
                f,
                wgpu::TextureFormat::Bgra8Unorm | wgpu::TextureFormat::Rgba8Unorm
            )
        })
        .or_else(|| caps.formats.first().copied())
        .ok_or_else(|| anyhow!("Wayland Vulkan surface exposed no usable formats"))?;
    let alpha_mode = [
        wgpu::CompositeAlphaMode::PreMultiplied,
        wgpu::CompositeAlphaMode::PostMultiplied,
        wgpu::CompositeAlphaMode::Auto,
        wgpu::CompositeAlphaMode::Opaque,
    ]
    .into_iter()
    .find(|mode| caps.alpha_modes.contains(mode))
    .unwrap_or(wgpu::CompositeAlphaMode::Opaque);
    let present_mode = if caps.present_modes.contains(&wgpu::PresentMode::Mailbox) {
        wgpu::PresentMode::Mailbox
    } else {
        wgpu::PresentMode::Fifo
    };

    let egui_ctx = egui::Context::default();
    let repaint_deadline = Arc::new(Mutex::new(Some(Instant::now())));
    let repaint_from_egui = Arc::clone(&repaint_deadline);
    egui_ctx.set_request_repaint_callback(move |info| {
        let deadline = Instant::now() + info.delay;
        let mut slot = repaint_from_egui
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none_or(|current| deadline < current) {
            *slot = Some(deadline);
        }
    });

    let app = HoardApp::new(&egui_ctx, start_path);
    let egui_renderer = egui_wgpu::Renderer::new(
        &device,
        surface_format,
        egui_wgpu::RendererOptions::default(),
    );

    let mut state = NativeWaylandApp {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        compositor_state: compositor,
        shm_state: shm,
        connection: conn.clone(),
        data_device_manager,
        window,
        app,
        egui_ctx,
        egui_renderer,
        device,
        queue,
        gpu_surface,
        surface_format,
        alpha_mode,
        present_mode,
        width: 1450,
        height: 900,
        scale_factor: 1,
        configured: false,
        exit: false,
        focused: false,
        modifiers: EguiModifiers::default(),
        sctk_modifiers: Modifiers::default(),
        pointer_down: false,
        last_pointer_serial: None,
        input_events: Vec::new(),
        repaint_deadline,
        needs_repaint: true,
        start_time: Instant::now(),
        presented_frames: 0,
        smoke_mode: std::env::var_os("HOARD_NATIVE_SMOKE").is_some(),
        seats: Vec::new(),
        armed_drag: None,
        drag_sources: Vec::new(),
        dnd_offers: Vec::new(),
        accept_serial: 1,
        loop_handle,
    };

    while !state.exit {
        // Keep the native host actively pumping/presenting while focused.
        // The event-driven-only scheduler can strand an otherwise healthy
        // Wayland/Vulkan client in a visually mapped but non-interactive state.
        // A bounded startup heartbeat guarantees initial async state becomes
        // visible before keyboard focus has necessarily arrived.
        let startup_active = state.start_time.elapsed() < Duration::from_secs(3);
        let active = state.focused || startup_active;
        let timeout = if active {
            Duration::from_millis(16)
        } else {
            state.next_dispatch_timeout()
        };
        event_loop
            .dispatch(timeout, &mut state)
            .context("native Wayland event dispatch failed")?;
        if state.take_due_repaint() {
            state.needs_repaint = true;
        }
        // Keep the focused event pump responsive without redrawing the entire UI at 60 FPS.
        // Pointer/wheel/key events, egui repaint requests, async work, and frame callbacks already
        // mark the surface dirty. Only the bounded startup heartbeat and smoke mode force frames.
        if startup_active || (state.smoke_mode && state.configured) {
            state.needs_repaint = true;
        }
        if state.configured && (state.needs_repaint || !state.input_events.is_empty()) {
            state.draw_if_needed(&qh)?;
        }
        if state.smoke_mode {
            if state.presented_frames >= 6
                && state.start_time.elapsed() >= Duration::from_millis(750)
                && state.app.runtime_smoke_ready()
            {
                let (path, entries) = state.app.runtime_smoke_summary();
                eprintln!(
                    "HOARD_NATIVE_SMOKE PASS presented={} path={} entries={}",
                    state.presented_frames,
                    path.display(),
                    entries
                );
                state.exit = true;
            } else if state.start_time.elapsed() > Duration::from_secs(10) {
                let (path, entries) = state.app.runtime_smoke_summary();
                return Err(anyhow!(
                    "native smoke timeout: presented={} configured={} path={} entries={}",
                    state.presented_frames,
                    state.configured,
                    path.display(),
                    entries
                ));
            }
        }
    }

    drop(state.gpu_surface);
    drop(state.window);
    Ok(())
}

struct NativeWaylandApp {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    compositor_state: CompositorState,
    shm_state: Shm,
    connection: Connection,
    data_device_manager: DataDeviceManagerState,
    window: Window,
    app: HoardApp,
    egui_ctx: egui::Context,
    egui_renderer: egui_wgpu::Renderer,
    device: wgpu::Device,
    queue: wgpu::Queue,
    gpu_surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
    alpha_mode: wgpu::CompositeAlphaMode,
    present_mode: wgpu::PresentMode,
    width: u32,
    height: u32,
    scale_factor: i32,
    configured: bool,
    exit: bool,
    focused: bool,
    modifiers: EguiModifiers,
    sctk_modifiers: Modifiers,
    pointer_down: bool,
    last_pointer_serial: Option<u32>,
    input_events: Vec<Event>,
    repaint_deadline: Arc<Mutex<Option<Instant>>>,
    needs_repaint: bool,
    start_time: Instant,
    presented_frames: u64,
    smoke_mode: bool,
    seats: Vec<SeatObjects>,
    armed_drag: Option<Vec<PathBuf>>,
    drag_sources: Vec<DragSourceState>,
    dnd_offers: Vec<(DragOffer, Vec<u8>, Option<RegistrationToken>)>,
    accept_serial: u32,
    loop_handle: LoopHandle<'static, NativeWaylandApp>,
}

impl NativeWaylandApp {
    fn next_dispatch_timeout(&self) -> Duration {
        if self.smoke_mode {
            return Duration::from_millis(16);
        }
        if self.needs_repaint || !self.input_events.is_empty() {
            return Duration::ZERO;
        }
        let now = Instant::now();
        let deadline = *self
            .repaint_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match deadline {
            Some(when) if when <= now => Duration::ZERO,
            Some(when) => when.duration_since(now).min(Duration::from_millis(50)),
            None => Duration::from_millis(50),
        }
    }

    fn take_due_repaint(&self) -> bool {
        let now = Instant::now();
        let mut slot = self
            .repaint_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_some_and(|deadline| deadline <= now) {
            *slot = None;
            true
        } else {
            false
        }
    }

    fn clear_repaint_deadline(&self) {
        *self
            .repaint_deadline
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = None;
    }

    fn configure_surface(&self) {
        if native_trace_enabled() {
            eprintln!(
                "HOARD_NATIVE configure={}x{} scale={} focused={}",
                self.width, self.height, self.scale_factor, self.focused
            );
        }
        self.gpu_surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.surface_format,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width: self.width.max(1),
                height: self.height.max(1),
                present_mode: self.present_mode,
                desired_maximum_frame_latency: 2,
                alpha_mode: self.alpha_mode,
                view_formats: vec![self.surface_format],
            },
        );
    }

    fn draw_if_needed(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        if !self.configured || !self.needs_repaint || self.width == 0 || self.height == 0 {
            return Ok(());
        }
        self.needs_repaint = false;
        self.clear_repaint_deadline();

        let pixels_per_point = self.scale_factor.max(1) as f32;
        let logical_width = self.width as f32 / pixels_per_point;
        let logical_height = self.height as f32 / pixels_per_point;
        let raw_input = RawInput {
            screen_rect: Some(Rect::from_min_size(
                Pos2::ZERO,
                Vec2::new(logical_width, logical_height),
            )),
            time: Some(self.start_time.elapsed().as_secs_f64()),
            predicted_dt: 1.0 / 60.0,
            max_texture_side: Some(self.device.limits().max_texture_dimension_2d as usize),
            events: std::mem::take(&mut self.input_events),
            focused: self.focused,
            ..Default::default()
        };

        let full_output = self.egui_ctx.run_ui(raw_input, |ui| self.app.update_ui(ui));
        self.process_platform_commands(qh);
        self.process_egui_platform_output(&full_output.platform_output);

        for (id, deltas) in &full_output.textures_delta.set {
            for delta in deltas {
                self.egui_renderer
                    .update_texture(&self.device, &self.queue, *id, delta);
            }
        }

        let paint_jobs = self
            .egui_ctx
            .tessellate(full_output.shapes, full_output.pixels_per_point);
        let screen_descriptor = egui_wgpu::ScreenDescriptor {
            size_in_pixels: [self.width.max(1), self.height.max(1)],
            pixels_per_point: full_output.pixels_per_point,
        };

        let frame = match self.gpu_surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame)
            | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => {
                self.needs_repaint = true;
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Outdated | wgpu::CurrentSurfaceTexture::Lost => {
                self.configure_surface();
                self.needs_repaint = true;
                return Ok(());
            }
            wgpu::CurrentSurfaceTexture::Validation => {
                return Err(anyhow!("Vulkan surface acquire validation failure"));
            }
        };

        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor {
                label: Some("Hoard egui encoder"),
            });
        let callback_commands = self.egui_renderer.update_buffers(
            &self.device,
            &self.queue,
            &mut encoder,
            &paint_jobs,
            &screen_descriptor,
        );
        {
            let render_pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Hoard egui render pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            let mut render_pass = render_pass.forget_lifetime();
            self.egui_renderer
                .render(&mut render_pass, &paint_jobs, &screen_descriptor);
        }

        self.queue.submit(
            callback_commands
                .into_iter()
                .chain(std::iter::once(encoder.finish())),
        );
        self.queue.present(frame);
        self.presented_frames = self.presented_frames.saturating_add(1);
        if native_trace_enabled()
            && (self.presented_frames <= 3 || self.presented_frames.is_multiple_of(120))
        {
            eprintln!(
                "HOARD_NATIVE presented={} size={}x{} focused={} repaint={}",
                self.presented_frames, self.width, self.height, self.focused, self.needs_repaint
            );
        }

        for id in &full_output.textures_delta.free {
            self.egui_renderer.free_texture(id);
        }

        Ok(())
    }

    fn ensure_seat_object(&mut self, qh: &QueueHandle<Self>, seat: &wl_seat::WlSeat) -> usize {
        if let Some(index) = self.seats.iter().position(|entry| entry.seat == *seat) {
            return index;
        }

        let data_device = self.data_device_manager.get_data_device(qh, seat);
        self.seats.push(SeatObjects {
            seat: seat.clone(),
            data_device,
            keyboard: None,
            pointer: None,
        });
        if native_trace_enabled() {
            eprintln!("HOARD_NATIVE seat-bound");
        }
        self.seats.len() - 1
    }

    fn process_platform_commands(&mut self, qh: &QueueHandle<Self>) {
        for command in self.app.take_platform_commands() {
            match command {
                PlatformCommand::BeginWindowMove => {
                    if let (Some(serial), Some(seat)) =
                        (self.last_pointer_serial, self.active_seat())
                    {
                        self.window.move_(&seat.seat, serial);
                    }
                }
                PlatformCommand::SetMaximized(true) => self.window.set_maximized(),
                PlatformCommand::SetMaximized(false) => self.window.unset_maximized(),
                PlatformCommand::Minimize => self.window.set_minimized(),
                PlatformCommand::Close => self.exit = true,
                PlatformCommand::ArmFileDrag(paths) => self.armed_drag = Some(paths),
                PlatformCommand::DisarmFileDrag => self.armed_drag = None,
            }
        }

        if self.pointer_down {
            self.maybe_start_external_drag(qh);
        }
    }

    fn map_egui_cursor(icon: egui::CursorIcon) -> Option<SctkCursorIcon> {
        Some(match icon {
            egui::CursorIcon::None => return None,
            egui::CursorIcon::Default => SctkCursorIcon::Default,
            egui::CursorIcon::ContextMenu => SctkCursorIcon::ContextMenu,
            egui::CursorIcon::Help => SctkCursorIcon::Help,
            egui::CursorIcon::PointingHand => SctkCursorIcon::Pointer,
            egui::CursorIcon::Progress => SctkCursorIcon::Progress,
            egui::CursorIcon::Wait => SctkCursorIcon::Wait,
            egui::CursorIcon::Cell => SctkCursorIcon::Cell,
            egui::CursorIcon::Crosshair => SctkCursorIcon::Crosshair,
            egui::CursorIcon::Text => SctkCursorIcon::Text,
            egui::CursorIcon::VerticalText => SctkCursorIcon::VerticalText,
            egui::CursorIcon::Alias => SctkCursorIcon::Alias,
            egui::CursorIcon::Copy => SctkCursorIcon::Copy,
            egui::CursorIcon::Move => SctkCursorIcon::Move,
            egui::CursorIcon::NoDrop => SctkCursorIcon::NoDrop,
            egui::CursorIcon::NotAllowed => SctkCursorIcon::NotAllowed,
            egui::CursorIcon::Grab => SctkCursorIcon::Grab,
            egui::CursorIcon::Grabbing => SctkCursorIcon::Grabbing,
            egui::CursorIcon::AllScroll => SctkCursorIcon::AllScroll,
            egui::CursorIcon::ResizeHorizontal => SctkCursorIcon::EwResize,
            egui::CursorIcon::ResizeNeSw => SctkCursorIcon::NeswResize,
            egui::CursorIcon::ResizeNwSe => SctkCursorIcon::NwseResize,
            egui::CursorIcon::ResizeVertical => SctkCursorIcon::NsResize,
            egui::CursorIcon::ResizeEast => SctkCursorIcon::EResize,
            egui::CursorIcon::ResizeSouthEast => SctkCursorIcon::SeResize,
            egui::CursorIcon::ResizeSouth => SctkCursorIcon::SResize,
            egui::CursorIcon::ResizeSouthWest => SctkCursorIcon::SwResize,
            egui::CursorIcon::ResizeWest => SctkCursorIcon::WResize,
            egui::CursorIcon::ResizeNorthWest => SctkCursorIcon::NwResize,
            egui::CursorIcon::ResizeNorth => SctkCursorIcon::NResize,
            egui::CursorIcon::ResizeNorthEast => SctkCursorIcon::NeResize,
            egui::CursorIcon::ResizeColumn => SctkCursorIcon::ColResize,
            egui::CursorIcon::ResizeRow => SctkCursorIcon::RowResize,
            egui::CursorIcon::ZoomIn => SctkCursorIcon::ZoomIn,
            egui::CursorIcon::ZoomOut => SctkCursorIcon::ZoomOut,
        })
    }

    fn process_egui_platform_output(&mut self, output: &egui::PlatformOutput) {
        if let Some(cursor) = Self::map_egui_cursor(output.cursor_icon) {
            for seat in &mut self.seats {
                if let Some(pointer) = seat.pointer.as_mut() {
                    let _ = pointer.set_cursor(&self.connection, cursor);
                }
            }
        }

        for command in &output.commands {
            match command {
                egui::OutputCommand::CopyText(text) => {
                    let Ok(mut child) = Command::new("wl-copy")
                        .arg("--type")
                        .arg("text/plain;charset=utf-8")
                        .stdin(Stdio::piped())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn()
                    else {
                        continue;
                    };
                    if let Some(mut stdin) = child.stdin.take() {
                        let _ = stdin.write_all(text.as_bytes());
                    }
                }
                egui::OutputCommand::OpenUrl(open) => {
                    let _ = Command::new("xdg-open")
                        .arg(&open.url)
                        .stdin(Stdio::null())
                        .stdout(Stdio::null())
                        .stderr(Stdio::null())
                        .spawn();
                }
                egui::OutputCommand::CopyImage(_) => {}
            }
        }
    }

    fn active_seat(&self) -> Option<&SeatObjects> {
        self.seats.first()
    }

    fn maybe_start_external_drag(&mut self, qh: &QueueHandle<Self>) {
        let Some(paths) = self.armed_drag.clone() else {
            return;
        };
        if paths.is_empty() || !self.drag_sources.is_empty() {
            return;
        }
        let Some(serial) = self.last_pointer_serial else {
            return;
        };
        let Some(data_device) = self.active_seat().map(|s| &s.data_device) else {
            return;
        };

        let source = self.data_device_manager.create_drag_and_drop_source(
            qh,
            [URI_LIST],
            DndAction::Copy | DndAction::Move,
        );
        source.start_drag(data_device, self.window.wl_surface(), None, serial);
        self.drag_sources.push(DragSourceState { source, paths });
        self.armed_drag = None;
    }

    fn queue_key_event(&mut self, event: &KeyEvent, pressed: bool, repeat: bool) {
        let key = egui_key(event.keysym, event.utf8.as_deref());
        let command_shortcut = pressed && self.modifiers.command;

        // egui text widgets consume the dedicated clipboard events. Keep the
        // ordinary key event as well so Hoard's file-level Ctrl+C/X/V
        // shortcuts still work whenever no text widget has focus.
        if command_shortcut && key == Some(Key::C) {
            self.input_events.push(Event::Copy);
        }
        if command_shortcut && key == Some(Key::X) {
            self.input_events.push(Event::Cut);
        }
        let paste_text = (command_shortcut && key == Some(Key::V))
            .then(read_wayland_clipboard_text)
            .flatten();
        if let Some(text) = paste_text {
            self.input_events.push(Event::Paste(text));
        }

        if let Some(key) = key {
            self.input_events.push(Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat,
                modifiers: self.modifiers,
            });
        }
        let text_input = (pressed && !self.modifiers.command && !self.modifiers.ctrl)
            .then_some(event.utf8.as_deref())
            .flatten()
            .filter(|text| !text.chars().any(char::is_control));
        if let Some(text) = text_input {
            self.input_events.push(Event::Text(text.to_owned()));
        }
        self.needs_repaint = true;
    }

    fn finish_incoming_drop(&mut self, data: Vec<u8>) {
        let text = String::from_utf8_lossy(&data);
        let mut paths = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let path = Url::parse(line)
                .ok()
                .filter(|url| {
                    url.scheme() == "file"
                        && (url.host_str().is_none() || url.host_str() == Some("localhost"))
                })
                .and_then(|url| url.to_file_path().ok());
            if let Some(path) = path {
                paths.push(path);
            }
        }
        self.app
            .accept_external_drop_paths(paths, self.modifiers.shift);
        self.needs_repaint = true;
    }
}

fn read_wayland_clipboard_text() -> Option<String> {
    // wl-paste providers do not all advertise the same text MIME.  Prefer the
    // explicit UTF-8 form Hoard writes, then accept ordinary text/plain, then
    // let wl-paste choose the provider's preferred textual representation.
    const TYPES: [Option<&str>; 3] = [Some("text/plain;charset=utf-8"), Some("text/plain"), None];
    for mime in TYPES {
        let mut command = Command::new("wl-paste");
        command.arg("--no-newline");
        if let Some(mime) = mime {
            command.args(["--type", mime]);
        }
        let output = command.stderr(Stdio::null()).output().ok()?;
        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).into_owned());
        }
    }
    None
}

fn uri_list(paths: &[PathBuf]) -> String {
    let mut out = String::new();
    for path in paths {
        if let Ok(url) = Url::from_file_path(path) {
            out.push_str(url.as_str());
            out.push_str("\r\n");
        }
    }
    out
}

fn egui_modifiers(mods: &Modifiers) -> EguiModifiers {
    EguiModifiers {
        alt: mods.alt,
        ctrl: mods.ctrl,
        shift: mods.shift,
        mac_cmd: false,
        command: mods.ctrl,
    }
}

fn egui_key(sym: Keysym, utf8: Option<&str>) -> Option<Key> {
    Some(match sym {
        Keysym::Escape => Key::Escape,
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        Keysym::Left | Keysym::KP_Left => Key::ArrowLeft,
        Keysym::Right | Keysym::KP_Right => Key::ArrowRight,
        Keysym::Up | Keysym::KP_Up => Key::ArrowUp,
        Keysym::Down | Keysym::KP_Down => Key::ArrowDown,
        Keysym::space | Keysym::KP_Space => Key::Space,
        Keysym::F1 => Key::F1,
        Keysym::F2 => Key::F2,
        Keysym::F3 => Key::F3,
        Keysym::F4 => Key::F4,
        Keysym::F5 => Key::F5,
        Keysym::F6 => Key::F6,
        Keysym::F7 => Key::F7,
        Keysym::F8 => Key::F8,
        Keysym::F9 => Key::F9,
        Keysym::F10 => Key::F10,
        Keysym::F11 => Key::F11,
        Keysym::F12 => Key::F12,
        Keysym::A | Keysym::a => Key::A,
        Keysym::B | Keysym::b => Key::B,
        Keysym::C | Keysym::c => Key::C,
        Keysym::D | Keysym::d => Key::D,
        Keysym::E | Keysym::e => Key::E,
        Keysym::F | Keysym::f => Key::F,
        Keysym::G | Keysym::g => Key::G,
        Keysym::H | Keysym::h => Key::H,
        Keysym::I | Keysym::i => Key::I,
        Keysym::J | Keysym::j => Key::J,
        Keysym::K | Keysym::k => Key::K,
        Keysym::L | Keysym::l => Key::L,
        Keysym::M | Keysym::m => Key::M,
        Keysym::N | Keysym::n => Key::N,
        Keysym::O | Keysym::o => Key::O,
        Keysym::P | Keysym::p => Key::P,
        Keysym::Q | Keysym::q => Key::Q,
        Keysym::R | Keysym::r => Key::R,
        Keysym::S | Keysym::s => Key::S,
        Keysym::T | Keysym::t => Key::T,
        Keysym::U | Keysym::u => Key::U,
        Keysym::V | Keysym::v => Key::V,
        Keysym::W | Keysym::w => Key::W,
        Keysym::X | Keysym::x => Key::X,
        Keysym::Y | Keysym::y => Key::Y,
        Keysym::Z | Keysym::z => Key::Z,
        _ => match utf8?.to_ascii_lowercase().as_str() {
            "a" => Key::A,
            "b" => Key::B,
            "c" => Key::C,
            "d" => Key::D,
            "e" => Key::E,
            "f" => Key::F,
            "g" => Key::G,
            "h" => Key::H,
            "i" => Key::I,
            "j" => Key::J,
            "k" => Key::K,
            "l" => Key::L,
            "m" => Key::M,
            "n" => Key::N,
            "o" => Key::O,
            "p" => Key::P,
            "q" => Key::Q,
            "r" => Key::R,
            "s" => Key::S,
            "t" => Key::T,
            "u" => Key::U,
            "v" => Key::V,
            "w" => Key::W,
            "x" => Key::X,
            "y" => Key::Y,
            "z" => Key::Z,
            "0" => Key::Num0,
            "1" => Key::Num1,
            "2" => Key::Num2,
            "3" => Key::Num3,
            "4" => Key::Num4,
            "5" => Key::Num5,
            "6" => Key::Num6,
            "7" => Key::Num7,
            "8" => Key::Num8,
            "9" => Key::Num9,
            _ => return None,
        },
    })
}

impl CompositorHandler for NativeWaylandApp {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        new_factor: i32,
    ) {
        if surface == self.window.wl_surface() {
            self.scale_factor = new_factor.max(1);
            surface.set_buffer_scale(self.scale_factor);
            self.needs_repaint = true;
        }
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        self.needs_repaint = true;
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for NativeWaylandApp {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for NativeWaylandApp {
    fn request_close(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _window: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _window: &Window,
        configure: WindowConfigure,
        _serial: u32,
    ) {
        let logical_width = configure.new_size.0.map_or(1450, |v| v.get()).max(1);
        let logical_height = configure.new_size.1.map_or(900, |v| v.get()).max(1);
        self.width = logical_width.saturating_mul(self.scale_factor.max(1) as u32);
        self.height = logical_height.saturating_mul(self.scale_factor.max(1) as u32);
        self.configured = true;
        self.configure_surface();
        self.needs_repaint = true;
    }
}

impl SeatHandler for NativeWaylandApp {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        let _ = self.ensure_seat_object(qh, &seat);
    }

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        // SCTK may report capabilities before Hoard's auxiliary per-seat object
        // has been populated via new_seat().  Do not drop those capability
        // events: create the seat object here as well, matching SCTK's
        // data-device example and making input registration order-independent.
        let index = self.ensure_seat_object(qh, &seat);
        match capability {
            Capability::Keyboard if self.seats[index].keyboard.is_none() => {
                if let Ok(keyboard) = self.seat_state.get_keyboard(qh, &seat, None) {
                    self.seats[index].keyboard = Some(keyboard);
                    if native_trace_enabled() {
                        eprintln!("HOARD_NATIVE keyboard-bound");
                    }
                }
            }
            Capability::Pointer if self.seats[index].pointer.is_none() => {
                let cursor_surface = self.compositor_state.create_surface(qh);
                if let Ok(pointer) = self.seat_state.get_pointer_with_theme::<_, ()>(
                    qh,
                    &seat,
                    self.shm_state.wl_shm(),
                    cursor_surface,
                    ThemeSpec::default(),
                ) {
                    self.seats[index].pointer = Some(pointer);
                    if native_trace_enabled() {
                        eprintln!("HOARD_NATIVE themed-pointer-bound");
                    }
                }
            }
            _ => {}
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        let Some(object) = self.seats.iter_mut().find(|s| s.seat == seat) else {
            return;
        };
        match capability {
            Capability::Keyboard => {
                if let Some(keyboard) = object.keyboard.take() {
                    keyboard.release();
                }
            }
            Capability::Pointer => {
                if let Some(pointer) = object.pointer.take() {
                    pointer.pointer().release();
                }
            }
            _ => {}
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.seats.retain(|s| s.seat != seat);
    }
}

impl KeyboardHandler for NativeWaylandApp {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
        if surface == self.window.wl_surface() {
            self.focused = true;
            if native_trace_enabled() {
                eprintln!("HOARD_NATIVE keyboard-focus=true");
            }
            self.input_events.push(Event::WindowFocused(true));
            self.needs_repaint = true;
        }
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        if surface == self.window.wl_surface() {
            self.focused = false;
            if native_trace_enabled() {
                eprintln!("HOARD_NATIVE keyboard-focus=false");
            }
            self.input_events.push(Event::WindowFocused(false));
            self.needs_repaint = true;
        }
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.queue_key_event(&event, true, false);
    }

    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.queue_key_event(&event, true, true);
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        self.queue_key_event(&event, false, false);
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _keyboard: &wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
        self.sctk_modifiers = modifiers;
        self.modifiers = egui_modifiers(&self.sctk_modifiers);
        self.input_events
            .push(Event::ModifiersChanged(self.modifiers));
        self.needs_repaint = true;
    }
}

impl PointerHandler for NativeWaylandApp {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        _pointer: &wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        use PointerEventKind::*;
        for event in events {
            if event.surface != *self.window.wl_surface() {
                continue;
            }
            let pos = Pos2::new(event.position.0 as f32, event.position.1 as f32);
            match event.kind {
                Enter { serial } => {
                    self.last_pointer_serial = Some(serial);
                    if native_trace_enabled() {
                        eprintln!("HOARD_NATIVE pointer-enter x={:.1} y={:.1}", pos.x, pos.y);
                    }
                    self.input_events.push(Event::PointerMoved(pos));
                }
                Leave { .. } => {
                    if self.pointer_down && self.armed_drag.is_some() {
                        self.maybe_start_external_drag(qh);
                    }
                    self.input_events.push(Event::PointerGone);
                }
                Motion { .. } => self.input_events.push(Event::PointerMoved(pos)),
                Press { button, serial, .. } => {
                    self.last_pointer_serial = Some(serial);
                    let button = match button {
                        BTN_LEFT => Some(PointerButton::Primary),
                        BTN_RIGHT => Some(PointerButton::Secondary),
                        BTN_MIDDLE => Some(PointerButton::Middle),
                        BTN_SIDE | BTN_BACK => Some(PointerButton::Extra1),
                        BTN_EXTRA | BTN_FORWARD => Some(PointerButton::Extra2),
                        _ => None,
                    };
                    if let Some(button) = button {
                        if button == PointerButton::Primary {
                            self.pointer_down = true;
                        }
                        self.input_events.push(Event::PointerButton {
                            pos,
                            button,
                            pressed: true,
                            modifiers: self.modifiers,
                        });
                    }
                }
                Release { button, serial, .. } => {
                    self.last_pointer_serial = Some(serial);
                    let button = match button {
                        BTN_LEFT => Some(PointerButton::Primary),
                        BTN_RIGHT => Some(PointerButton::Secondary),
                        BTN_MIDDLE => Some(PointerButton::Middle),
                        BTN_SIDE | BTN_BACK => Some(PointerButton::Extra1),
                        BTN_EXTRA | BTN_FORWARD => Some(PointerButton::Extra2),
                        _ => None,
                    };
                    if let Some(button) = button {
                        if button == PointerButton::Primary {
                            self.pointer_down = false;
                            self.armed_drag = None;
                        }
                        self.input_events.push(Event::PointerButton {
                            pos,
                            button,
                            pressed: false,
                            modifiers: self.modifiers,
                        });
                    }
                }
                Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    // Match Tiamat's native Wayland wheel semantics: physical/notched
                    // wheels use logical egui line steps (including high-resolution
                    // value120 metadata) with the shared 1.4x feel multiplier. True
                    // continuous axes such as touchpads remain unscaled point deltas.
                    let wheel_component = |value120: i32, discrete: i32| {
                        let steps = if value120 != 0 {
                            value120 as f32 / 120.0
                        } else {
                            discrete as f32
                        };
                        -steps * WAYLAND_WHEEL_LINE_MULTIPLIER
                    };
                    let line_delta = Vec2::new(
                        wheel_component(horizontal.value120, horizontal.discrete),
                        wheel_component(vertical.value120, vertical.discrete),
                    );
                    if line_delta != Vec2::ZERO {
                        self.input_events.push(Event::MouseWheel {
                            unit: egui::MouseWheelUnit::Line,
                            delta: line_delta,
                            modifiers: self.modifiers,
                            phase: egui::TouchPhase::Move,
                        });
                    } else {
                        let point_delta =
                            Vec2::new(-(horizontal.absolute as f32), -(vertical.absolute as f32));
                        if point_delta != Vec2::ZERO {
                            self.input_events.push(Event::MouseWheel {
                                unit: egui::MouseWheelUnit::Point,
                                delta: point_delta,
                                modifiers: self.modifiers,
                                phase: egui::TouchPhase::Move,
                            });
                        }
                    }
                }
            }
            self.needs_repaint = true;
        }
    }
}

impl DataDeviceHandler for NativeWaylandApp {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
        _surface: &wl_surface::WlSurface,
    ) {
        let Some(device) = self
            .seats
            .iter()
            .find(|s| s.data_device.inner() == data_device)
        else {
            return;
        };
        let Some(offer) = device.data_device.data().drag_offer() else {
            return;
        };
        if offer.with_mime_types(|types| types.iter().any(|m| m == URI_LIST)) {
            self.accept_serial = self.accept_serial.wrapping_add(1).max(1);
            offer.accept_mime_type(self.accept_serial, Some(URI_LIST.to_owned()));
            offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        }
    }

    fn leave(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _data_device: &WlDataDevice) {}

    fn motion(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
        _x: f64,
        _y: f64,
    ) {
    }

    fn selection(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _data_device: &WlDataDevice,
    ) {
    }

    fn drop_performed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        data_device: &WlDataDevice,
    ) {
        let Some(device) = self
            .seats
            .iter()
            .find(|s| s.data_device.inner() == data_device)
        else {
            return;
        };
        let Some(offer) = device.data_device.data().drag_offer() else {
            return;
        };
        if !offer.with_mime_types(|types| types.iter().any(|m| m == URI_LIST)) {
            return;
        }

        let Ok(read_pipe) = offer.receive(URI_LIST.to_owned()) else {
            return;
        };
        self.accept_serial = self.accept_serial.wrapping_add(1).max(1);
        offer.accept_mime_type(self.accept_serial, Some(URI_LIST.to_owned()));
        offer.set_actions(DndAction::Copy | DndAction::Move, DndAction::Copy);
        self.dnd_offers.push((offer.clone(), Vec::new(), None));
        let tracked_offer = offer.clone();
        match self
            .loop_handle
            .insert_source(read_pipe, move |_, fd, state| {
                let Some(index) = state
                    .dnd_offers
                    .iter()
                    .position(|entry| entry.0 == tracked_offer)
                else {
                    return PostAction::Remove;
                };
                let (_, mut bytes, token) = state.dnd_offers.remove(index);
                let Some(token) = token else {
                    state.dnd_offers.push((tracked_offer.clone(), bytes, None));
                    return PostAction::Continue;
                };
                let file: &mut fs::File = unsafe { fd.get_mut() };
                let mut reader = BufReader::new(file);
                match reader.fill_buf() {
                    Ok([]) => {
                        tracked_offer.finish();
                        tracked_offer.destroy();
                        state.finish_incoming_drop(bytes);
                        PostAction::Remove
                    }
                    Ok(buf) => {
                        let consumed = buf.len();
                        bytes.extend_from_slice(buf);
                        reader.consume(consumed);
                        state
                            .dnd_offers
                            .push((tracked_offer.clone(), bytes, Some(token)));
                        PostAction::Continue
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {
                        state
                            .dnd_offers
                            .push((tracked_offer.clone(), bytes, Some(token)));
                        PostAction::Continue
                    }
                    Err(_) => {
                        tracked_offer.finish();
                        tracked_offer.destroy();
                        PostAction::Remove
                    }
                }
            }) {
            Ok(token) => {
                if let Some(entry) = self.dnd_offers.last_mut() {
                    entry.2 = Some(token);
                }
            }
            Err(_) => {
                offer.finish();
                offer.destroy();
                self.dnd_offers.retain(|entry| entry.0 != offer);
            }
        }
    }
}

impl DataOfferHandler for NativeWaylandApp {
    fn source_actions(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        offer: &mut DragOffer,
        actions: DndAction,
    ) {
        let preferred = if actions.contains(DndAction::Move) {
            DndAction::Move
        } else {
            DndAction::Copy
        };
        offer.set_actions(DndAction::Copy | DndAction::Move, preferred);
    }

    fn selected_action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _offer: &mut DragOffer,
        _actions: DndAction,
    ) {
    }
}

impl DataSourceHandler for NativeWaylandApp {
    fn accept_mime(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _mime: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        source: &WlDataSource,
        mime: String,
        write_pipe: WritePipe,
    ) {
        if mime != URI_LIST {
            return;
        }
        let Some(paths) = self
            .drag_sources
            .iter()
            .find(|entry| entry.source.inner() == source)
            .map(|entry| entry.paths.clone())
        else {
            return;
        };
        let mut file = fs::File::from(OwnedFd::from(write_pipe));
        let _ = file.write_all(uri_list(&paths).as_bytes());
    }

    fn cancelled(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        self.drag_sources
            .retain(|entry| entry.source.inner() != source);
        source.destroy();
    }

    fn dnd_dropped(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _source: &WlDataSource) {
    }

    fn dnd_finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, source: &WlDataSource) {
        // The target owns the filesystem operation. Hoard never deletes source
        // files merely because a Wayland DnD action negotiated MOVE.
        self.drag_sources
            .retain(|entry| entry.source.inner() != source);
        source.destroy();
    }

    fn action(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _source: &WlDataSource,
        _action: DndAction,
    ) {
    }
}

impl ShmHandler for NativeWaylandApp {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm_state
    }
}

impl ProvidesRegistryState for NativeWaylandApp {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_registry!(NativeWaylandApp);
smithay_client_toolkit::delegate_dispatch2!(NativeWaylandApp);
