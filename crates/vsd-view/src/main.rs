//! `vsd-view` — the native VSD viewer (ROADMAP 4a).
//!
//! Minimal by intent: winit + softbuffer + the project's own
//! rasterizer; no GPU stack, no toolkit. What makes it a *VSD* viewer:
//!
//! - the verification banner is first-class UI — validation, signature
//!   verification, and layout recomputation run on open, and the result
//!   is the first thing on screen;
//! - search is exact (display-list text runs, not raster heuristics)
//!   and highlights are computed from the same metrics the layout
//!   engine used;
//! - Ctrl+C copies the page's real text.
//!
//! Keys: ←/→ PgUp/PgDn Home/End pages · +/- zoom, 0 fit · / search,
//! Enter commit, Esc cancel, n next match · Ctrl+C copy page text · q quit.

mod vm;

use std::num::NonZeroU32;
use std::rc::Rc;

use anyhow::{Context, Result};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, KeyEvent, Modifiers, WindowEvent};
use winit::event_loop::{ActiveEventLoop, EventLoop};
use winit::keyboard::{Key, NamedKey};
use winit::window::{Window, WindowId};

use vsd_core::layout::Page;
use vsd_core::Document;

const BANNER_H: u32 = 32;

fn main() {
    if let Err(e) = run() {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let path = std::env::args()
        .nth(1)
        .context("usage: vsd-view <file.vsd>")?;
    let file = vsd_container::read_file(&path, &vsd_container::ReadOptions::default())
        .with_context(|| format!("reading {path}"))?;

    let document = file.document;
    let pages: Vec<Page> = match document.render_cache()? {
        Some(cache) => cache
            .pages
            .iter()
            .map(|id| Ok(Page::from_value(&document.store.get_value(id)?)?))
            .collect::<Result<_>>()?,
        None => vsd_layout::layout_document(&document, &vsd_layout::LayoutOptions::default())?,
    };
    anyhow::ensure!(!pages.is_empty(), "document has no pages");
    let banner = vm::make_banner(&document, &file.signatures);

    let event_loop = EventLoop::new()?;
    let mut app = App {
        title: path,
        document,
        pages,
        banner,
        state: None,
        view: vm::ViewState::new(0),
        modifiers: Modifiers::default(),
    };
    app.view = vm::ViewState::new(app.pages.len());
    event_loop.run_app(&mut app)?;
    Ok(())
}

struct Gfx {
    window: Rc<Window>,
    surface: softbuffer::Surface<Rc<Window>, Rc<Window>>,
}

struct App {
    title: String,
    document: Document,
    pages: Vec<Page>,
    banner: vm::Banner,
    state: Option<Gfx>,
    view: vm::ViewState,
    modifiers: Modifiers,
}

impl App {
    fn redraw(&mut self) {
        let Some(gfx) = &mut self.state else { return };
        let size = gfx.window.inner_size();
        let (w, h) = (size.width.max(1), size.height.max(1));

        let mut frame = match tiny_skia::Pixmap::new(w, h) {
            Some(p) => p,
            None => return,
        };
        frame.fill(tiny_skia::Color::from_rgba8(0x60, 0x60, 0x66, 0xff));

        // --- Page ---------------------------------------------------------
        let page = &self.pages[self.view.page.min(self.pages.len() - 1)];
        let avail_h = h.saturating_sub(BANNER_H).max(1);
        let fit = (w as f64 / page.width_mm).min(avail_h as f64 / page.height_mm);
        let px_per_mm = (fit * self.view.zoom).max(0.05);
        let dpi = px_per_mm * 25.4;
        if let Ok(mut rendered) = vsd_render::render_page(&self.document, page, dpi.min(600.0)) {
            // Search highlights, from the same metrics the engine used.
            for hit in vm::page_highlights(page, &self.view.query) {
                vsd_render::fill_rect_px(
                    &mut rendered,
                    hit.x_mm * px_per_mm,
                    hit.y_mm * px_per_mm,
                    hit.w_mm * px_per_mm,
                    hit.h_mm * px_per_mm,
                    [0xff, 0xe2, 0x3e, 0x80],
                );
            }
            let x = ((w as i32 - rendered.width() as i32) / 2).max(0);
            frame.draw_pixmap(
                x,
                BANNER_H as i32,
                rendered.as_ref(),
                &tiny_skia::PixmapPaint::default(),
                tiny_skia::Transform::identity(),
                None,
            );
        }

        // --- Banner --------------------------------------------------------
        let (bg, fg) = if self.banner.ok {
            ([0x1e, 0x56, 0x31, 0xff], [0xff, 0xff, 0xff, 0xff])
        } else {
            ([0xb3, 0x26, 0x1e, 0xff], [0xff, 0xff, 0xff, 0xff])
        };
        vsd_render::fill_rect_px(&mut frame, 0.0, 0.0, w as f64, BANNER_H as f64, bg);
        let status = match &self.view.input {
            Some(q) => format!("search: {q}_"),
            None => format!(
                "{}  ·  page {}/{}{}",
                self.banner.text,
                self.view.page + 1,
                self.pages.len(),
                if self.view.query.is_empty() {
                    String::new()
                } else {
                    format!(
                        "  ·  \"{}\" on {} page(s) (n: next)",
                        self.view.query,
                        self.view.matches.len()
                    )
                }
            ),
        };
        vsd_render::draw_label(&mut frame, 10.0, 21.0, 14.0, fg, &status);

        // --- Blit -----------------------------------------------------------
        if gfx
            .surface
            .resize(NonZeroU32::new(w).unwrap(), NonZeroU32::new(h).unwrap())
            .is_err()
        {
            return;
        }
        if let Ok(mut buffer) = gfx.surface.buffer_mut() {
            for (dst, px) in buffer.iter_mut().zip(frame.pixels()) {
                let p = px.demultiply();
                *dst = (p.red() as u32) << 16 | (p.green() as u32) << 8 | p.blue() as u32;
            }
            let _ = buffer.present();
        }
    }

    fn key(&mut self, event: KeyEvent, el: &ActiveEventLoop) {
        if event.state != ElementState::Pressed {
            return;
        }
        // Search input mode captures characters first.
        if let Some(buf) = &mut self.view.input {
            match &event.logical_key {
                Key::Named(NamedKey::Escape) => self.view.input = None,
                Key::Named(NamedKey::Backspace) => {
                    buf.pop();
                }
                Key::Named(NamedKey::Enter) => {
                    let pages = std::mem::take(&mut self.pages);
                    self.view.commit_search(&pages);
                    self.pages = pages;
                }
                Key::Character(s) => buf.push_str(s),
                Key::Named(NamedKey::Space) => buf.push(' '),
                _ => {}
            }
            self.request_redraw();
            return;
        }
        match &event.logical_key {
            Key::Named(NamedKey::ArrowRight | NamedKey::PageDown | NamedKey::Space) => {
                self.view.next_page()
            }
            Key::Named(NamedKey::ArrowLeft | NamedKey::PageUp) => self.view.prev_page(),
            Key::Named(NamedKey::Home) => self.view.page = 0,
            Key::Named(NamedKey::End) => self.view.page = self.pages.len() - 1,
            Key::Character(c) => match c.as_str() {
                "+" | "=" => self.view.zoom_in(),
                "-" => self.view.zoom_out(),
                "0" => self.view.zoom_reset(),
                "/" => self.view.input = Some(String::new()),
                "n" => self.view.next_match(),
                "q" => el.exit(),
                "c" if self.modifiers.state().control_key() => {
                    let text = vm::page_text(&self.pages[self.view.page]);
                    if let Ok(mut clipboard) = arboard::Clipboard::new() {
                        let _ = clipboard.set_text(text);
                    }
                }
                _ => {}
            },
            Key::Named(NamedKey::Escape) => {
                self.view.query.clear();
                self.view.matches.clear();
            }
            _ => {}
        }
        self.request_redraw();
    }

    fn request_redraw(&self) {
        if let Some(gfx) = &self.state {
            gfx.window.request_redraw();
        }
    }
}

impl ApplicationHandler for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        let attrs = Window::default_attributes()
            .with_title(format!("vsd-view — {}", self.title))
            .with_inner_size(winit::dpi::LogicalSize::new(900.0, 1100.0));
        let window = Rc::new(event_loop.create_window(attrs).expect("create window"));
        let context = softbuffer::Context::new(window.clone()).expect("softbuffer context");
        let surface = softbuffer::Surface::new(&context, window.clone()).expect("surface");
        self.state = Some(Gfx { window, surface });
        self.request_redraw();
    }

    fn window_event(&mut self, el: &ActiveEventLoop, _id: WindowId, event: WindowEvent) {
        match event {
            WindowEvent::CloseRequested => el.exit(),
            WindowEvent::RedrawRequested => self.redraw(),
            WindowEvent::Resized(_) => self.request_redraw(),
            WindowEvent::ModifiersChanged(m) => self.modifiers = m,
            WindowEvent::KeyboardInput { event, .. } => self.key(event, el),
            _ => {}
        }
    }
}
