//! Fernsteuerung für automatische Prüfungen beim Entwickeln: spielt Maus- und Tastatureingaben
//! aus einem kleinen Skript ab (`--script "click 300,200; key F5; text abc"`).

use std::collections::VecDeque;

use eframe::egui::{Event, Key, Modifiers, PointerButton, Pos2};

pub struct Step {
    pub events: Vec<Event>,
    pub modifiers: Modifiers,
    /// Zusätzliche Leerframes nach diesem Schritt
    pub wait: u32,
    /// Danach so viele Millisekunden warten – für Ergebnisse, die aus dem Hintergrund kommen
    pub sleep_ms: u64,
    /// Während dieser Wartezeit zeichnet das Programm nicht von selbst neu, sondern nur, wenn es – wie im normalen
    /// Betrieb – von einem Ereignis geweckt wird
    pub idle: bool,
    /// Ab diesem Schritt gilt das Fenster als im Vordergrund (`true`) oder im Hintergrund – unabhängig davon, ob
    /// das Prüffenster wirklich den Fokus von Windows hat
    pub focus: Option<bool>,
}

fn step(events: Vec<Event>, modifiers: Modifiers) -> Step {
    Step { events, modifiers, wait: 0, sleep_ms: 0, idle: false, focus: None }
}

fn pos(text: &str) -> Option<Pos2> {
    let (x, y) = text.split_once(',')?;
    Some(Pos2::new(x.trim().parse().ok()?, y.trim().parse().ok()?))
}

fn button(pos: Pos2, button: PointerButton, pressed: bool, modifiers: Modifiers) -> Event {
    Event::PointerButton { pos, button, pressed, modifiers }
}

fn click(out: &mut VecDeque<Step>, at: Pos2, which: PointerButton, mods: Modifiers, times: usize) {
    out.push_back(step(vec![Event::PointerMoved(at)], mods));
    for _ in 0..times {
        out.push_back(step(vec![button(at, which, true, mods)], mods));
        out.push_back(step(vec![button(at, which, false, mods)], mods));
    }
}

fn key_combo(text: &str) -> Option<(Key, Modifiers)> {
    let mut mods = Modifiers::NONE;
    let mut key = None;
    for part in text.split('+') {
        match part.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "strg" => {
                mods.ctrl = true;
                mods.command = true;
            }
            "shift" | "umschalt" => mods.shift = true,
            "alt" => mods.alt = true,
            _ => key = Key::from_name(part.trim()),
        }
    }
    key.map(|k| (k, mods))
}

pub fn parse(script: &str) -> VecDeque<Step> {
    let mut out = VecDeque::new();
    for command in script.split(';') {
        let command = command.trim();
        let (name, rest) = command.split_once(' ').unwrap_or((command, ""));
        let rest = rest.trim();
        match name {
            "click" => {
                if let Some(p) = pos(rest) {
                    click(&mut out, p, PointerButton::Primary, Modifiers::NONE, 1);
                }
            }
            "dclick" => {
                if let Some(p) = pos(rest) {
                    click(&mut out, p, PointerButton::Primary, Modifiers::NONE, 2);
                }
            }
            "rclick" => {
                if let Some(p) = pos(rest) {
                    click(&mut out, p, PointerButton::Secondary, Modifiers::NONE, 1);
                }
            }
            "mclick" => {
                if let Some(p) = pos(rest) {
                    click(&mut out, p, PointerButton::Middle, Modifiers::NONE, 1);
                }
            }
            "cclick" => {
                if let Some(p) = pos(rest) {
                    let mods = Modifiers { ctrl: true, command: true, ..Modifiers::NONE };
                    click(&mut out, p, PointerButton::Primary, mods, 1);
                }
            }
            "sclick" => {
                if let Some(p) = pos(rest) {
                    click(&mut out, p, PointerButton::Primary, Modifiers::SHIFT, 1);
                }
            }
            "move" => {
                if let Some(p) = pos(rest) {
                    out.push_back(step(vec![Event::PointerMoved(p)], Modifiers::NONE));
                }
            }
            // drag x1,y1 x2,y2 – endet mit „drop", wenn losgelassen werden soll
            "drag" => {
                let mut parts = rest.split_whitespace();
                if let (Some(a), Some(b)) = (parts.next().and_then(pos), parts.next().and_then(pos)) {
                    out.push_back(step(vec![Event::PointerMoved(a)], Modifiers::NONE));
                    out.push_back(step(vec![button(a, PointerButton::Primary, true, Modifiers::NONE)], Modifiers::NONE));
                    for i in 1..=8 {
                        let t = i as f32 / 8.0;
                        out.push_back(step(vec![Event::PointerMoved(a + (b - a) * t)], Modifiers::NONE));
                    }
                }
            }
            "drop" => {
                if let Some(p) = pos(rest) {
                    out.push_back(step(vec![button(p, PointerButton::Primary, false, Modifiers::NONE)], Modifiers::NONE));
                }
            }
            "key" => {
                if let Some((key, mods)) = key_combo(rest) {
                    let ev = |pressed| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: mods };
                    out.push_back(step(vec![ev(true)], mods));
                    out.push_back(step(vec![ev(false)], mods));
                }
            }
            // Mausrad an der aktuellen Mausposition: „wheel 3" dreht nach oben, „cwheel -2" mit gedrückter Strg-Taste nach unten
            "wheel" | "cwheel" => {
                let mods = if name == "cwheel" { Modifiers { ctrl: true, command: true, ..Modifiers::NONE } } else { Modifiers::NONE };
                let lines: f32 = rest.parse().unwrap_or(1.0);
                let wheel = Event::MouseWheel {
                    unit: eframe::egui::MouseWheelUnit::Line,
                    delta: eframe::egui::Vec2::new(0.0, lines),
                    phase: eframe::egui::TouchPhase::Move,
                    modifiers: mods,
                };
                out.push_back(step(vec![wheel], mods));
            }
            "text" => out.push_back(step(vec![Event::Text(rest.to_owned())], Modifiers::NONE)),
            "copy" => out.push_back(step(vec![Event::Copy], Modifiers::NONE)),
            // So kommt Umschalt+Entf unter Windows tatsächlich an: als „Ausschneiden" mit gedrückter Umschalttaste.
            "shiftdel" => out.push_back(step(vec![Event::Cut], Modifiers::SHIFT)),
            "cut" => out.push_back(step(vec![Event::Cut], Modifiers { ctrl: true, command: true, ..Modifiers::NONE })),
            "wait" => {
                let frames = rest.parse().unwrap_or(5);
                out.push_back(Step { wait: frames, ..step(Vec::new(), Modifiers::NONE) });
            }
            // „blur" tut so, als wechsle der Benutzer in ein anderes Programm, „focus", als kehre er zurück.
            "blur" | "focus" => out.push_back(Step { focus: Some(name == "focus"), ..step(Vec::new(), Modifiers::NONE) }),
            // „sleep 800" wartet nach der Uhr statt nach Bildern: Bilder entstehen je nach Rechner verschieden schnell.
            // „idle 800" wartet genauso lange, zeichnet in der Zeit aber nicht ständig neu: so verhält sich das
            // Programm im normalen Betrieb, wenn niemand etwas tut. Was dann von selbst geschehen soll (ein Ordner
            // aktualisiert sich, weil sich auf der Platte etwas geändert hat), lässt sich nur so prüfen.
            "sleep" | "idle" => {
                let sleep_ms = rest.parse().unwrap_or(500);
                out.push_back(Step { sleep_ms, idle: name == "idle", ..step(Vec::new(), Modifiers::NONE) });
            }
            _ => {}
        }
    }
    out
}

/// Echte Mausbewegungen würden das Skript stören.
pub fn is_real_pointer_event(e: &Event) -> bool {
    matches!(
        e,
        Event::PointerMoved(_) | Event::MouseMoved(_) | Event::PointerButton { .. } | Event::PointerGone | Event::MouseWheel { .. }
    )
}
