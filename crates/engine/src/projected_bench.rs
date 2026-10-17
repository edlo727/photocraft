//! QA benchmark (ignored): `PC_BENCH_W=20000 cargo test --release -p photocraft-engine --lib
//! projected_bench -- --ignored --nocapture`. A synthetic F32 RGBA latlong, timings and RSS.

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use photocraft_geom::Rect;
    use serde_json::json;

    use crate::Session;
    use crate::equirect::{equirect_to_dir, rotation_between};

    fn rss_mb() -> f64 {
        std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|s| s.lines().find(|l| l.starts_with("VmRSS")).and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok()))
            .unwrap_or(0.0)
            / 1024.0
    }

    fn t<R>(label: &str, f: impl FnOnce() -> R) -> R {
        let s = Instant::now();
        let r = f();
        println!("BENCH {label:<44} {:>9.1} ms   RSS {:>8.0} MB", s.elapsed().as_secs_f64() * 1000.0, rss_mb());
        r
    }

    #[test]
    #[ignore]
    fn projected_bench() {
        let w: u32 = std::env::var("PC_BENCH_W").ok().and_then(|v| v.parse().ok()).unwrap_or(4096);
        let h = w / 2;
        println!("BENCH size {w}x{h} F32 RGBA, start RSS {:.0} MB", rss_mb());
        let mut s = Session::new();
        t("file.new (32-bit)", || s.execute("file.new", json!({"width": w, "height": h, "depth": 32})).unwrap());
        let id = s.active().unwrap().active_layer.unwrap();
        t("fill synthetic HDR content (band writes)", || {
            s.edit("fill", |doc, _| {
                let surf = doc.layer_mut(id).unwrap().surface_mut().unwrap();
                let n = surf.channels();
                let band = 256;
                let mut y0 = 0;
                while y0 < h as i32 {
                    let y1 = (y0 + band).min(h as i32);
                    let mut d = vec![0f32; w as usize * (y1 - y0) as usize * n];
                    for (i, px) in d.chunks_exact_mut(n).enumerate() {
                        let x = (i % w as usize) as f32;
                        let y = (y0 as usize + i / w as usize) as f32;
                        px[0] = 0.2 + x / w as f32 + if y < h as f32 * 0.2 { 8.0 } else { 0.0 };
                        px[1] = 0.3 + 0.1 * (x * 0.01).sin();
                        px[2] = 0.4;
                        if n == 4 {
                            px[3] = 1.0;
                        }
                    }
                    surf.write_region(Rect::new(0, y0, w as i32, y1), &d);
                    y0 = y1;
                }
                Ok(())
            })
            .unwrap()
        });
        let view = json!({"yaw": 20.0, "pitch": 5.0, "fov": 90.0, "width": 1920, "height": 1080});
        let vp = crate::brush_cmds::projected::bench_view(&view);
        t("view resample 1920x1080 (to_view)", || {
            let st = s.active().unwrap();
            let l = st.doc.layer(id).unwrap().surface().unwrap();
            crate::brush_cmds::projected::to_view(l, &vp, Rect::new(0, 0, 1920, 1080), w as i32, h as i32)
        });
        t("projected brush stroke + write-back", || {
            s.execute("paint.projectedStroke", json!({"points": [[400, 500], [900, 520], [1400, 600]], "size": 80, "hardness": 80, "projection": view})).unwrap()
        });
        t("projected spot healing", || {
            s.execute("paint.projectedTool", json!({"command": "paint.spotHealing", "params": {"type": "proximityMatch", "target": "pixels"}, "projection": view, "points": [[900, 540, 1.0], [920, 540, 1.0]]})).unwrap()
        });
        t("projected gradient (full sphere)", || {
            s.execute("paint.projectedTool", json!({"command": "paint.gradient", "params": {"target": "pixels", "colors": ["#000000", "#ffffff"], "opacity": 50}, "projection": view, "points": [[960, 700, 1.0], [960, 300, 1.0]]})).unwrap()
        });
        t("projected paint bucket (whole latlong)", || {
            s.execute("paint.projectedTool", json!({"command": "paint.bucket", "params": {"target": "pixels", "tolerance": 30, "contiguous": true, "contents": "foreground"}, "projection": view, "points": [[960, 900, 1.0]]})).unwrap()
        });
        let q = rotation_between(equirect_to_dir(f64::from(w) / 2.0, f64::from(h) / 2.0, f64::from(w), f64::from(h)), equirect_to_dir(f64::from(w) / 2.0 + f64::from(w) / 10.0, f64::from(h) / 2.0, f64::from(w), f64::from(h)));
        t("whole-layer projected Move (36 deg yaw)", || s.execute("layer.projectedRotate", json!({"rotation": q.concat()})).unwrap());
        s.execute("select.rect", json!({"x": w / 2, "y": h / 2, "width": w / 20, "height": h / 20})).unwrap();
        t("selection projected Move", || s.execute("layer.projectedRotate", json!({"rotation": q.concat()})).unwrap());
        s.execute("select.deselect", json!({})).unwrap();
        // Flat (non-projection) equivalents.
        t("flat brush stroke", || s.execute("paint.stroke", json!({"points": [[w / 4, h / 2], [w / 2, h / 2 + 50]], "size": 80, "hardness": 80})).unwrap());
        t("flat gradient (whole doc)", || s.execute("paint.gradient", json!({"from": [0, 0], "to": [w, h], "colors": ["#000000", "#ffffff"], "opacity": 50})).unwrap());
        t("flat paint bucket", || s.execute("paint.bucket", json!({"x": 10, "y": h - 10, "tolerance": 30})).unwrap());
        t("undo x3", || {
            for _ in 0..3 {
                s.execute("edit.undo", json!({})).unwrap();
            }
        });
        let bad = {
            let st = s.active().unwrap();
            let l = st.doc.layer(id).unwrap().surface().unwrap();
            let r = l.read_region(Rect::new(0, 0, w as i32, 64));
            r.iter().filter(|v| v.is_nan()).count()
        };
        println!("BENCH NaN in top rows after edits: {bad}");
        println!("BENCH peak RSS (VmHWM) {}", std::fs::read_to_string("/proc/self/status").unwrap().lines().find(|l| l.starts_with("VmHWM")).unwrap_or(""));
    }
}
