//! Mapa toků Kubernetes laboratoře: komponenty ve sloupcích podle vrstvy, šipky podle
//! skutečných spojení (conntrack), přerušovaně povolené cesty bez provozu.

use super::*;
use crate::k8s::flows::{Edge, Node};
use crate::k8s::model::Pod;
use crate::k8s::policy::{Peer, RuleSet};
use std::collections::{BTreeMap, BTreeSet};

/// Hrana mapy po sloučení služeb: `fe → svc/api → api` se kreslí jako `fe → api` přes `api`.
#[derive(Debug, Clone, PartialEq)]
pub struct MapEdge {
    pub from: Node,
    pub to: Node,
    pub port: u16,
    /// služba, přes kterou spojení šlo
    pub via: Option<String>,
    pub live: u32,
    pub recent: u32,
}

/// Sloučí hrany přes služby: X → Service(s) a Service(s) → Y dá X → Y (port cíle).
pub fn collapse(edges: &[Edge]) -> Vec<MapEdge> {
    let mut out = Vec::new();
    for e in edges {
        match (&e.from, &e.to) {
            (_, Node::Service(s)) => {
                let backends: Vec<&Edge> = edges.iter().filter(|b| b.from == Node::Service(s.clone())).collect();
                if backends.is_empty() {
                    out.push(MapEdge { from: e.from.clone(), to: e.to.clone(), port: e.port, via: None, live: e.live, recent: e.recent });
                }
                for b in backends {
                    out.push(MapEdge { from: e.from.clone(), to: b.to.clone(), port: b.port, via: Some(s.clone()), live: e.live, recent: e.recent });
                }
            }
            (Node::Service(_), _) => {}
            _ => out.push(MapEdge { from: e.from.clone(), to: e.to.clone(), port: e.port, via: None, live: e.live, recent: e.recent }),
        }
    }
    // stejné dvojice (např. dvě služby na stejný pod) sečíst
    let mut acc: Vec<MapEdge> = Vec::new();
    for e in out {
        match acc.iter_mut().find(|a| a.from == e.from && a.to == e.to && a.port == e.port) {
            Some(a) => {
                a.live += e.live;
                a.recent += e.recent;
            }
            None => acc.push(e),
        }
    }
    acc
}

fn tier_rank(t: &str) -> Option<usize> {
    match t.to_lowercase().as_str() {
        "frontend" | "fe" | "web" | "ui" => Some(1),
        "backend" | "api" | "be" | "app" => Some(2),
        "database" | "db" | "data" | "cache" | "storage" => Some(3),
        _ => None,
    }
}

/// Sloupce mapy: localhost vždy první, komponenty podle labelu `tier` (frontend → backend →
/// database), jinak podle hloubky v grafu (zdroje vlevo), DNS a cíle mimo cluster poslední.
pub fn layout_columns(pods: &[Pod], edges: &[MapEdge]) -> Vec<Vec<Node>> {
    let mut comps: BTreeMap<String, Option<usize>> = BTreeMap::new();
    for p in pods {
        let r = p.labels.get("tier").and_then(|t| tier_rank(t));
        let e = comps.entry(p.component.clone()).or_insert(r);
        if e.is_none() {
            *e = r;
        }
    }
    for e in edges {
        for n in [&e.from, &e.to] {
            if let Node::Component(c) = n {
                comps.entry(c.clone()).or_insert(None);
            }
        }
    }
    // hloubka pro komponenty bez vrstvy: nejdelší cesta od zdroje (max. 6 kol, cykly neškodí)
    let mut depth: BTreeMap<String, usize> = comps.keys().map(|c| (c.clone(), 1)).collect();
    for _ in 0..6 {
        for e in edges {
            if let (Node::Component(a), Node::Component(b)) = (&e.from, &e.to) {
                let da = comps.get(a).copied().flatten().unwrap_or(depth[a]);
                if comps.get(b).copied().flatten().is_none() && depth[b] < da + 1 && da < 8 {
                    depth.insert(b.clone(), da + 1);
                }
            }
        }
    }
    let mut cols: BTreeMap<usize, Vec<Node>> = BTreeMap::new();
    if edges.iter().any(|e| e.from == Node::Localhost) {
        cols.entry(0).or_default().push(Node::Localhost);
    }
    for (c, r) in &comps {
        cols.entry(r.unwrap_or(depth[c])).or_default().push(Node::Component(c.clone()));
    }
    let last = cols.keys().max().copied().unwrap_or(0) + 1;
    let mut tail: BTreeSet<Node> = BTreeSet::new();
    for e in edges {
        for n in [&e.from, &e.to] {
            if matches!(n, Node::Dns | Node::Outside(_) | Node::Service(_)) {
                tail.insert(n.clone());
            }
        }
    }
    if !tail.is_empty() {
        cols.insert(last, tail.into_iter().collect());
    }
    cols.into_values().filter(|c| !c.is_empty()).collect()
}

/// Povolené cesty z pravidel, po kterých zatím nic neteče (kreslí se přerušovaně).
pub fn allowed_idle(rules: &RuleSet, edges: &[MapEdge]) -> Vec<(Node, Node, u16)> {
    rules
        .rules
        .iter()
        .map(|r| {
            let from = match &r.from {
                Peer::Component(c) => Node::Component(c.clone()),
                Peer::Localhost => Node::Localhost,
            };
            (from, Node::Component(r.to.clone()), r.port)
        })
        .filter(|(f, t, p)| !edges.iter().any(|e| &e.from == f && &e.to == t && e.port == *p))
        .collect()
}

fn dashed(p: &egui::Painter, a: egui::Pos2, b: egui::Pos2, stroke: egui::Stroke) {
    let len = a.distance(b);
    let dir = (b - a) / len.max(1.0);
    let mut t = 0.0;
    while t < len {
        let e = (t + 6.0).min(len);
        p.line_segment([a + dir * t, a + dir * e], stroke);
        t += 11.0;
    }
}

fn arrow(p: &egui::Painter, a: egui::Pos2, b: egui::Pos2, stroke: egui::Stroke) {
    let dir = (b - a).normalized();
    let n = egui::vec2(-dir.y, dir.x);
    let s = 7.0 + stroke.width;
    p.add(egui::Shape::convex_polygon(vec![b, b - dir * s + n * s * 0.55, b - dir * s - n * s * 0.55], stroke.color, egui::Stroke::NONE));
}

/// Nakreslí mapu (zámek u komponent, když platí default deny).
pub fn draw(ui: &mut egui::Ui, pods: &[Pod], edges: &[MapEdge], rules: &RuleSet) {
    let cols = layout_columns(pods, edges);
    if cols.is_empty() {
        ui.label(muted(t!("Zatím žádné pody ani spojení.", "No pods or connections yet.")));
        return;
    }
    let rows = cols.iter().map(|c| c.len()).max().unwrap_or(1) as f32;
    let w = ui.available_width().max(400.0);
    let h = (rows * 74.0 + 30.0).max(180.0);
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::hover());
    let p = ui.painter_at(rect);
    let (bw, bh) = (150.0_f32.min(w / cols.len() as f32 - 30.0), 40.0);
    let colw = w / cols.len() as f32;
    let mut pos: BTreeMap<Node, egui::Rect> = BTreeMap::new();
    for (ci, col) in cols.iter().enumerate() {
        let gap = h / col.len() as f32;
        for (ri, n) in col.iter().enumerate() {
            let c = egui::pos2(rect.left() + colw * (ci as f32 + 0.5), rect.top() + gap * (ri as f32 + 0.5));
            pos.insert(n.clone(), egui::Rect::from_center_size(c, egui::vec2(bw, bh)));
        }
    }
    let font = egui::TextStyle::Small.resolve(ui.style());
    let edge_line = |r1: egui::Rect, r2: egui::Rect| {
        let (a, b) = (r1.center(), r2.center());
        let dir = (b - a).normalized();
        // průsečík s okrajem obdélníku (přibližně: polovina šířky/výšky podle směru)
        let off = |r: egui::Rect| {
            let tx = if dir.x.abs() > 1e-3 { r.width() / 2.0 / dir.x.abs() } else { f32::INFINITY };
            let ty = if dir.y.abs() > 1e-3 { r.height() / 2.0 / dir.y.abs() } else { f32::INFINITY };
            tx.min(ty) + 3.0
        };
        (a + dir * off(r1), b - dir * off(r2))
    };
    let mut hover: Option<String> = None;
    for (f, t, port) in allowed_idle(rules, edges) {
        if let (Some(r1), Some(r2)) = (pos.get(&f), pos.get(&t)) {
            let (a, b) = edge_line(*r1, *r2);
            let st = egui::Stroke::new(1.2_f32, theme::green().gamma_multiply(0.7));
            dashed(&p, a, b, st);
            arrow(&p, a, b, st);
            p.text(a.lerp(b, 0.5) + egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, format!(":{port}"), font.clone(), theme::green());
        }
    }
    for e in edges {
        let (Some(r1), Some(r2)) = (pos.get(&e.from), pos.get(&e.to)) else { continue };
        let (a, b) = edge_line(*r1, *r2);
        let color = if e.live > 0 { theme::accent() } else { theme::muted() };
        let width = 1.5 + ((e.live + e.recent) as f32).ln_1p().min(4.0);
        let st = egui::Stroke::new(width, color);
        p.line_segment([a, b], st);
        arrow(&p, a, b, st);
        let mid = a.lerp(b, 0.5);
        p.text(mid - egui::vec2(0.0, 10.0), egui::Align2::CENTER_CENTER, format!(":{}", e.port), font.clone(), color);
        if resp.hover_pos().is_some_and(|hp| egui::Rect::from_two_pos(a, b).expand(8.0).contains(hp) && mid.distance(hp) < 40.0) {
            let via = e.via.as_ref().map(|v| tf!(" přes službu {v}", " via service {v}")).unwrap_or_default();
            hover = Some(tf!(
                "{} → {} :{}{via}\nživá spojení: {}\nnedávná (do ~2 min): {}",
                "{} → {} :{}{via}\nlive connections: {}\nrecent (last ~2 min): {}",
                e.from.label(),
                e.to.label(),
                e.port,
                e.live,
                e.recent
            ));
        }
    }
    for (n, r) in &pos {
        let (fill, stroke) = match n {
            Node::Localhost => (theme::accent().gamma_multiply(0.18), theme::accent()),
            Node::Component(_) => (theme::card(), theme::card_stroke()),
            _ => (theme::bg(), theme::card_stroke()),
        };
        p.rect_filled(*r, 6.0, fill);
        p.rect_stroke(*r, 6.0, egui::Stroke::new(1.0_f32, stroke), egui::StrokeKind::Inside);
        let mut label = n.label();
        if let Node::Component(c) = n {
            let n_pods = pods.iter().filter(|p| &p.component == c).count();
            let ready = pods.iter().filter(|p| &p.component == c && p.ready.0 == p.ready.1 && p.ready.1 > 0).count();
            if rules.deny_all {
                label = format!("\u{E2FA} {label}");
            }
            p.text(r.center() + egui::vec2(0.0, 9.0), egui::Align2::CENTER_CENTER, tf!("{ready}/{n_pods} podů", "{ready}/{n_pods} pods"), font.clone(), theme::muted());
            p.text(r.center() - egui::vec2(0.0, 7.0), egui::Align2::CENTER_CENTER, label, egui::TextStyle::Body.resolve(ui.style()), ui.visuals().text_color());
        } else {
            p.text(r.center(), egui::Align2::CENTER_CENTER, label, font.clone(), ui.visuals().text_color());
        }
    }
    if let Some(t) = hover {
        resp.on_hover_text(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pod(c: &str, tier: Option<&str>) -> Pod {
        let mut p = Pod { name: format!("{c}-1"), component: c.into(), ..Default::default() };
        if let Some(t) = tier {
            p.labels.insert("tier".into(), t.into());
        }
        p
    }

    fn e(from: Node, to: Node, port: u16) -> Edge {
        Edge { from, to, port, live: 1, recent: 2 }
    }

    fn c(s: &str) -> Node {
        Node::Component(s.into())
    }

    #[test]
    fn collapse_through_service() {
        let edges = vec![
            e(c("fe"), Node::Service("api".into()), 3000),
            e(Node::Service("api".into()), c("api"), 3000),
            e(c("api"), c("db"), 5432),
            e(c("api"), Node::Service("orphan".into()), 80),
        ];
        let m = collapse(&edges);
        assert_eq!(m.len(), 3);
        assert_eq!((m[0].from.clone(), m[0].to.clone(), m[0].via.as_deref()), (c("fe"), c("api"), Some("api")));
        assert_eq!((m[0].live, m[0].recent), (1, 2));
        assert_eq!(m[2].to, Node::Service("orphan".into()), "služba bez backendu zůstane");
    }

    #[test]
    fn layout_columns_by_tier() {
        let pods = vec![pod("db", Some("database")), pod("fe", Some("frontend")), pod("api", Some("backend"))];
        let m = collapse(&[e(Node::Localhost, Node::Service("fe".into()), 30080), e(Node::Service("fe".into()), c("fe"), 80), e(c("api"), Node::Dns, 53)]);
        let cols = layout_columns(&pods, &m);
        assert_eq!(cols, vec![vec![Node::Localhost], vec![c("fe")], vec![c("api")], vec![c("db")], vec![Node::Dns]]);
    }

    #[test]
    fn layout_columns_without_tier_follow_edges() {
        let pods = vec![pod("z-client", None), pod("a-server", None), pod("m-mid", None)];
        let m = collapse(&[e(c("z-client"), c("m-mid"), 1), e(c("m-mid"), c("a-server"), 2)]);
        let cols = layout_columns(&pods, &m);
        assert_eq!(cols, vec![vec![c("z-client")], vec![c("m-mid")], vec![c("a-server")]]);
    }

    #[test]
    fn idle_allowed_paths() {
        let rules = RuleSet::demo();
        let m = collapse(&[e(c("fe"), Node::Service("api".into()), 3000), e(Node::Service("api".into()), c("api"), 3000)]);
        let idle = allowed_idle(&rules, &m);
        assert_eq!(idle, vec![(Node::Localhost, c("fe"), 80), (c("api"), c("db"), 5432)]);
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use crate::k8s::flows::Edge;

    #[test]
    fn draw_headless_does_not_panic() {
        let mut pods = Vec::new();
        for (c, t) in [("fe", "frontend"), ("api", "backend"), ("db", "database"), ("worker", "x")] {
            let mut p = Pod { name: format!("{c}-1"), component: c.into(), ready: (1, 1), ..Default::default() };
            p.labels.insert("tier".into(), t.into());
            pods.push(p);
        }
        let c = |s: &str| Node::Component(s.into());
        let edges = collapse(&[
            Edge { from: Node::Localhost, to: Node::Service("fe".into()), port: 30080, live: 1, recent: 0 },
            Edge { from: Node::Service("fe".into()), to: c("fe"), port: 80, live: 1, recent: 0 },
            Edge { from: c("fe"), to: c("api"), port: 3000, live: 0, recent: 5 },
            Edge { from: c("api"), to: Node::Dns, port: 53, live: 3, recent: 0 },
            Edge { from: c("api"), to: Node::Outside("1.2.3.4".into()), port: 443, live: 1, recent: 0 },
            Edge { from: c("db"), to: c("db"), port: 1, live: 1, recent: 0 },
        ]);
        let ctx = egui::Context::default();
        for rules in [RuleSet::default(), RuleSet::demo()] {
            let _ = ctx.run(egui::RawInput::default(), |ctx| {
                egui::CentralPanel::default().show(ctx, |ui| {
                    draw(ui, &pods, &edges, &rules);
                    draw(ui, &[], &[], &rules);
                });
            });
        }
    }
}
