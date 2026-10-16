use crate::{
    hints::{Action, Hints, Input},
    layout::{self, Label},
    model::{Candidate, Output, Snapshot},
    render::Renderer,
};
use anyhow::{Context, Result, ensure};
use calloop::EventLoop;
use calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState, FrameCallbackData},
    delegate_registry,
    dispatch2::Dispatch2,
    output::{OutputHandler, OutputInfo, OutputState},
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    shell::{
        WaylandSurface,
        wlr_layer::{
            Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
            LayerSurfaceConfigure,
        },
    },
    shm::{
        Shm, ShmHandler,
        slot::{Buffer, SlotPool},
    },
};
use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_callback, wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
};
use wayland_protocols::wp::{
    fractional_scale::v1::client::{
        wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1,
        wp_fractional_scale_v1::{self, WpFractionalScaleV1},
    },
    viewporter::client::{wp_viewport::WpViewport, wp_viewporter::WpViewporter},
};

struct Ignore;
impl<I: Proxy> Dispatch2<I, State> for Ignore {
    fn event(&self, _: &mut State, _: &I, _: I::Event, _: &Connection, _: &QueueHandle<State>) {}
}
struct ScaleData(usize);
impl Dispatch2<WpFractionalScaleV1, State> for ScaleData {
    fn event(
        &self,
        state: &mut State,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event
            && let Some(slot) = state.surfaces.get_mut(self.0)
        {
            if scale == 0 || scale > 16 * 120 {
                state.fail("invalid fractional scale from compositor");
                return;
            }
            slot.scale = scale as f64 / 120.0;
            slot.dirty = true;
        }
    }
}
struct SyncData;
impl Dispatch2<wl_callback::WlCallback, State> for SyncData {
    fn event(
        &self,
        state: &mut State,
        _: &wl_callback::WlCallback,
        _: wl_callback::Event,
        _: &Connection,
        _: &QueueHandle<State>,
    ) {
        state.synced = true;
    }
}

#[derive(Clone, PartialEq)]
struct OutputSignature {
    position: Option<(i32, i32)>,
    size: Option<(i32, i32)>,
    scale: i32,
    transform: wl_output::Transform,
}
impl From<&OutputInfo> for OutputSignature {
    fn from(info: &OutputInfo) -> Self {
        Self {
            position: info.logical_position,
            size: info.logical_size,
            scale: info.scale_factor,
            transform: info.transform,
        }
    }
}

struct Surface {
    layer: LayerSurface,
    output: wl_output::WlOutput,
    signature: OutputSignature,
    viewport: Option<WpViewport>,
    fractional: Option<WpFractionalScaleV1>,
    size: Option<(u32, u32)>,
    scale: f64,
    buffer: Option<Buffer>,
    dirty: bool,
    frame_pending: bool,
}

struct State {
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    shm: Shm,
    pool: SlotPool,
    surfaces: Vec<Surface>,
    monitors: Vec<Output>,
    windows: Vec<Candidate>,
    hints: Hints,
    labels: Vec<Vec<Label>>,
    renderer: Renderer,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    keyboard_focus: bool,
    held: HashSet<u32>,
    modifiers: Modifiers,
    result: Option<Option<usize>>,
    error: Option<anyhow::Error>,
    cleaning: bool,
    synced: bool,
}

pub(crate) fn select(snapshot: &Snapshot, cancelled: Arc<AtomicBool>) -> Result<Option<usize>> {
    let conn = Connection::connect_to_env().context("cannot connect to Wayland")?;
    let (globals, mut queue) = registry_queue_init::<State>(&conn)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh).context("wl_compositor unavailable")?;
    let layer_shell = LayerShell::bind(&globals, &qh).context("layer-shell unavailable")?;
    let shm = Shm::bind(&globals, &qh).context("wl_shm unavailable")?;
    let pool = SlotPool::new(4096, &shm)?;
    let viewport = globals.bind::<WpViewporter, _, _>(&qh, 1..=1, Ignore).ok();
    let fractional = globals
        .bind::<WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, Ignore)
        .ok();
    let mut state = State {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        shm,
        pool,
        surfaces: Vec::new(),
        monitors: snapshot.outputs.clone(),
        windows: snapshot.windows.clone(),
        hints: Hints::new(snapshot.windows.len())?,
        labels: Vec::new(),
        renderer: Renderer::new()?,
        keyboard: None,
        keyboard_focus: false,
        held: HashSet::new(),
        modifiers: Modifiers::default(),
        result: None,
        error: None,
        cleaning: false,
        synced: false,
    };
    // Initial registry/output events occur before there is a keyboard-grabbing surface.
    queue.roundtrip(&mut state)?;
    queue.roundtrip(&mut state)?;
    if let Some(error) = state.error.take() {
        return Err(error);
    }
    ensure!(state.keyboard.is_some(), "Wayland seat has no keyboard");
    let keyboard_output = state.monitors.iter().position(|o| o.focused).unwrap_or(0);
    let available: Vec<_> = state.outputs.outputs().collect();
    for i in 0..state.monitors.len() {
        let output = available
            .iter()
            .find(|o| {
                state
                    .outputs
                    .info(o)
                    .is_some_and(|info| info.name.as_ref() == Some(&state.monitors[i].name))
            })
            .context("Hyprland output not found in Wayland registry")?
            .clone();
        let info = state
            .outputs
            .info(&output)
            .context("missing Wayland output information")?;
        if let Some((x, y)) = info.logical_position {
            ensure!(
                x as f64 == state.monitors[i].rect.x && y as f64 == state.monitors[i].rect.y,
                "output position changed during startup; retry"
            );
        }
        if let Some((w, h)) = info.logical_size {
            ensure!(w > 0 && h > 0, "invalid Wayland output size");
            state.monitors[i].rect.w = w as f64;
            state.monitors[i].rect.h = h as f64;
        }
        let surface = compositor.create_surface(&qh);
        let region = compositor.wl_compositor().create_region(&qh, Ignore);
        surface.set_input_region(Some(&region));
        region.destroy();
        let layer = layer_shell.create_layer_surface(
            &qh,
            surface,
            Layer::Overlay,
            Some("hyprselector"),
            Some(&output),
        );
        layer.set_anchor(Anchor::TOP | Anchor::BOTTOM | Anchor::LEFT | Anchor::RIGHT);
        // Client coordinates are relative to the full output, not its usable
        // work area. Zero would shift/shrink this surface around panels such as
        // Waybar; -1 keeps its origin at the output's top-left edge.
        layer.set_exclusive_zone(-1);
        layer.set_size(0, 0);
        layer.set_keyboard_interactivity(if i == keyboard_output {
            KeyboardInteractivity::Exclusive
        } else {
            KeyboardInteractivity::None
        });
        let vp = viewport
            .as_ref()
            .map(|v| v.get_viewport(layer.wl_surface(), &qh, Ignore));
        let fs = if vp.is_some() {
            fractional
                .as_ref()
                .map(|f| f.get_fractional_scale(layer.wl_surface(), &qh, ScaleData(i)))
        } else {
            None
        };
        let scale = if vp.is_some() {
            state.monitors[i].scale
        } else {
            info.scale_factor.max(1) as f64
        };
        layer.commit();
        state.surfaces.push(Surface {
            layer,
            output,
            signature: OutputSignature::from(&info),
            viewport: vp,
            fractional: fs,
            size: None,
            scale,
            buffer: None,
            dirty: true,
            frame_pending: false,
        });
    }
    state.labels = layout::place(
        &state.windows,
        &state.monitors,
        &state.hints,
        &state.renderer.font,
    )?;
    let mut event_loop: EventLoop<State> = EventLoop::try_new()?;
    WaylandSource::new(conn.clone(), queue)
        .insert(event_loop.handle())
        .map_err(|e| anyhow::anyhow!("cannot register Wayland event source: {e}"))?;
    let started = Instant::now();
    let operation = (|| -> Result<()> {
        while state.result.is_none() && state.error.is_none() {
            if cancelled.load(Ordering::Relaxed) {
                state.result = Some(None);
                break;
            }
            event_loop.dispatch(Duration::from_millis(100), &mut state)?;
            state.draw(&qh)?;
            ensure!(
                state.ready() || started.elapsed() < Duration::from_secs(3),
                "compositor did not configure overlays or grant keyboard focus"
            );
        }
        if let Some(error) = state.error.take() {
            return Err(error);
        }
        Ok(())
    })();
    state.cleaning = true;
    for slot in state.surfaces.drain(..) {
        slot.layer
            .set_keyboard_interactivity(KeyboardInteractivity::None);
        slot.layer.commit();
        if let Some(f) = slot.fractional {
            f.destroy();
        }
        if let Some(v) = slot.viewport {
            v.destroy();
        }
        // SCTK drops the layer role before destroying the wl_surface.
        drop(slot.layer);
    }
    if let Some(keyboard) = state.keyboard.take() {
        keyboard.release();
    }
    state.synced = false;
    conn.display().sync(&qh, SyncData);
    let deadline = Instant::now() + Duration::from_secs(2);
    let cleanup = (|| -> Result<()> {
        conn.flush()?;
        while !state.synced {
            ensure!(
                Instant::now() < deadline,
                "Wayland cleanup acknowledgement timed out"
            );
            event_loop.dispatch(Duration::from_millis(50), &mut state)?;
        }
        Ok(())
    })();
    // On any cleanup error, disconnecting also destroys all surfaces server-side.
    operation?;
    cleanup.context("failed to release Wayland overlays")?;
    Ok(state.result.flatten())
}

impl State {
    fn fail(&mut self, message: impl Into<String>) {
        if !self.cleaning && self.error.is_none() {
            self.error = Some(anyhow::anyhow!(message.into()));
        }
    }
    fn ready(&self) -> bool {
        self.keyboard_focus
            && !self.surfaces.is_empty()
            && self
                .surfaces
                .iter()
                .all(|s| s.size.is_some() && s.buffer.is_some())
    }
    fn draw(&mut self, qh: &QueueHandle<Self>) -> Result<()> {
        for (i, slot) in self.surfaces.iter_mut().enumerate() {
            if !slot.dirty || slot.frame_pending {
                continue;
            }
            let Some((w, h)) = slot.size else {
                continue;
            };
            ensure!(
                slot.scale.is_finite() && slot.scale > 0.0 && slot.scale <= 16.0,
                "invalid buffer scale"
            );
            let scale = if slot.viewport.is_some() {
                slot.scale
            } else {
                slot.scale.ceil()
            };
            let bw = (w as f64 * scale).ceil();
            let bh = (h as f64 * scale).ceil();
            ensure!(
                bw <= 16384.0 && bh <= 16384.0,
                "output buffer dimensions too large"
            );
            let (bw, bh) = (bw as i32, bh as i32);
            let pixels = self.renderer.draw(
                &self.monitors[i],
                (bw as u32, bh as u32),
                scale,
                &self.windows,
                &self.labels[i],
                &self.hints,
            )?;
            let reuse = slot
                .buffer
                .as_ref()
                .filter(|b| b.stride() == bw * 4 && b.height() == bh)
                .and_then(|b| self.pool.canvas(b));
            if let Some(canvas) = reuse {
                canvas.copy_from_slice(&pixels);
            } else {
                let (buffer, canvas) =
                    self.pool
                        .create_buffer(bw, bh, bw * 4, wl_shm::Format::Argb8888)?;
                canvas.copy_from_slice(&pixels);
                slot.buffer = Some(buffer);
            }
            let surface = slot.layer.wl_surface();
            if let Some(viewport) = &slot.viewport {
                surface.set_buffer_scale(1);
                viewport.set_destination(w as i32, h as i32);
            } else {
                surface.set_buffer_scale(scale as i32);
            }
            slot.buffer
                .as_ref()
                .context("missing output buffer")?
                .attach_to(surface)?;
            surface.damage_buffer(0, 0, bw, bh);
            surface.frame(qh, FrameCallbackData(surface.clone()));
            slot.layer.commit();
            slot.frame_pending = true;
            slot.dirty = false;
        }
        Ok(())
    }
}

impl CompositorHandler for State {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if let Some(slot) = self
            .surfaces
            .iter_mut()
            .find(|s| s.layer.wl_surface() == surface)
            && slot.viewport.is_none()
        {
            slot.scale = factor.max(1) as f64;
            slot.dirty = true;
        }
    }
    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _: u32,
    ) {
        if let Some(slot) = self
            .surfaces
            .iter_mut()
            .find(|s| s.layer.wl_surface() == surface)
        {
            slot.frame_pending = false;
        }
    }
    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}
impl OutputHandler for State {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }
    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if !self.surfaces.is_empty() {
            self.fail("output added during selection; retry");
        }
    }
    fn update_output(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        if let Some(slot) = self.surfaces.iter().find(|s| s.output == output)
            && self
                .outputs
                .info(&output)
                .is_some_and(|i| OutputSignature::from(&i) != slot.signature)
        {
            self.fail("output geometry changed during selection; retry");
        }
    }
    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {
        if !self.surfaces.is_empty() {
            self.fail("output removed during selection; retry");
        }
    }
}
impl LayerShellHandler for State {
    fn closed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &LayerSurface) {
        self.fail("overlay closed by compositor; retry");
    }
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _: u32,
    ) {
        let Some(i) = self.surfaces.iter().position(|s| s.layer == *layer) else {
            return;
        };
        let size = configure.new_size;
        if size.0 == 0 || size.1 == 0 {
            self.fail("compositor supplied an empty overlay size");
            return;
        }
        if self.surfaces[i].size.is_some_and(|old| old != size) {
            self.fail("output size changed during selection; retry");
            return;
        }
        self.surfaces[i].size = Some(size);
        self.surfaces[i].dirty = true;
        self.monitors[i].rect.w = size.0 as f64;
        self.monitors[i].rect.h = size.1 as f64;
        match layout::place(
            &self.windows,
            &self.monitors,
            &self.hints,
            &self.renderer.font,
        ) {
            Ok(labels) => self.labels = labels,
            Err(e) => self.error = Some(e),
        }
    }
}
impl SeatHandler for State {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }
    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            match self.seats.get_keyboard(qh, &seat, None) {
                Ok(k) => self.keyboard = Some(k),
                Err(e) => self.fail(format!("cannot initialize keyboard: {e}")),
            }
        }
    }
    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(k) = self.keyboard.take() {
                k.release();
            }
            self.fail("keyboard removed during selection");
        }
    }
    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {
        self.fail("Wayland seat removed during selection");
    }
}
impl KeyboardHandler for State {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        surface: &wl_surface::WlSurface,
        _: u32,
        raw: &[u32],
        _: &[Keysym],
    ) {
        self.keyboard_focus = self
            .surfaces
            .iter()
            .any(|s| s.layer.wl_surface() == surface);
        self.held = raw.iter().copied().collect();
    }
    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.keyboard_focus = false;
        self.held.clear();
        if !self.cleaning && self.result.is_none() {
            self.fail("overlay lost keyboard focus; retry");
        }
    }
    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        if !self.held.insert(event.raw_code) || !self.ready() || self.result.is_some() {
            return;
        }
        let input = if event.keysym == Keysym::Escape {
            Some(Input::Escape)
        } else if self.modifiers.ctrl || self.modifiers.alt || self.modifiers.logo {
            None
        } else if event.keysym == Keysym::BackSpace {
            Some(Input::Backspace)
        } else {
            event
                .utf8
                .as_deref()
                .filter(|s| s.len() == 1)
                .and_then(|s| s.chars().next())
                .filter(|c| c.is_ascii_alphabetic())
                .map(Input::Letter)
        };
        if let Some(input) = input {
            match self.hints.input(input) {
                Action::Cancelled => self.result = Some(None),
                Action::Selected(i) => self.result = Some(Some(i)),
                Action::Redraw => {
                    for slot in &mut self.surfaces {
                        slot.dirty = true;
                    }
                }
            }
        }
    }
    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }
    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.held.remove(&event.raw_code);
    }
    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}
impl ShmHandler for State {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}
delegate_registry!(State);
impl ProvidesRegistryState for State {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}
smithay_client_toolkit::delegate_dispatch2!(State);
