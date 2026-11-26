// Das Programmsymbol wird berechnet statt als Datei mitgeliefert. Diese Datei wird sowohl vom
// Programm (Fenstersymbol) als auch von build.rs (Symbol der exe) eingebunden – daher ohne Abhängigkeiten.

fn sd_round_rect(px: f32, py: f32, half_w: f32, half_h: f32, radius: f32) -> f32 {
    let qx = px.abs() - half_w + radius;
    let qy = py.abs() - half_h + radius;
    let outside = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    outside + qx.max(qy).min(0.0) - radius
}

fn icon_sample(x: f32, y: f32) -> [f32; 4] {
    if sd_round_rect(x - 0.5, y - 0.5, 0.47, 0.47, 0.21) > 0.0 {
        return [0.0; 4];
    }
    // Blauer Verlauf von oben nach unten
    let top = [0.40, 0.60, 0.98];
    let bottom = [0.17, 0.36, 0.86];
    let mut c = [
        top[0] + (bottom[0] - top[0]) * y,
        top[1] + (bottom[1] - top[1]) * y,
        top[2] + (bottom[2] - top[2]) * y,
    ];
    // Zwei Fenster übereinander: das obere hell (aktiv), das untere zurückgenommen
    let upper = sd_round_rect(x - 0.5, y - 0.335, 0.29, 0.125, 0.055) <= 0.0;
    let lower = sd_round_rect(x - 0.5, y - 0.665, 0.29, 0.125, 0.055) <= 0.0;
    if upper {
        c = [0.99, 0.995, 1.0];
        // Markierte Zeile im oberen Fenster
        if sd_round_rect(x - 0.44, y - 0.335, 0.16, 0.028, 0.028) <= 0.0 {
            c = [0.30, 0.50, 0.95];
        }
    } else if lower {
        let a = 0.62;
        c = [c[0] + (1.0 - c[0]) * a, c[1] + (1.0 - c[1]) * a, c[2] + (1.0 - c[2]) * a];
    }
    [c[0], c[1], c[2], 1.0]
}

/// RGBA-Pixel (nicht vormultipliziert) des Programmsymbols in der gewünschten Kantenlänge.
pub fn render_icon(size: u32) -> Vec<u8> {
    const SS: u32 = 4;
    let mut out = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let mut acc = [0.0f32; 4];
            for sy in 0..SS {
                for sx in 0..SS {
                    let px = (x as f32 + (sx as f32 + 0.5) / SS as f32) / size as f32;
                    let py = (y as f32 + (sy as f32 + 0.5) / SS as f32) / size as f32;
                    let s = icon_sample(px, py);
                    acc[0] += s[0] * s[3];
                    acc[1] += s[1] * s[3];
                    acc[2] += s[2] * s[3];
                    acc[3] += s[3];
                }
            }
            let n = (SS * SS) as f32;
            let alpha = acc[3] / n;
            let i = ((y * size + x) * 4) as usize;
            if alpha > 0.0 {
                out[i] = (acc[0] / acc[3] * 255.0).round() as u8;
                out[i + 1] = (acc[1] / acc[3] * 255.0).round() as u8;
                out[i + 2] = (acc[2] / acc[3] * 255.0).round() as u8;
                out[i + 3] = (alpha * 255.0).round() as u8;
            }
        }
    }
    out
}
