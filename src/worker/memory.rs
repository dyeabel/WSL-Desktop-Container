//! Vzorkování paměti WSL pro přehled: plný vzorek při každém obnovení, když je přehled
//! vidět, jinak jen minimální vzorek do historie zhruba jednou za minutu.

use super::{Msg, Worker};
use crate::docker::StatRow;
use crate::history::Sample;
use crate::memory::{self, FreeEvent, MemNow, MemPoint};
use crate::wsl;
use std::sync::atomic::Ordering;
use std::thread;

impl Worker {
    /// Volá se při každém obnovení. Plný vzorek s kontejnery dělá vlákno statistik
    /// (`memory_sample(Some(rows))`); tady zbytek – bez enginu, nebo minimální vzorek.
    pub(super) fn memory_tick(&self, distro_running: bool, stats_thread: bool) {
        if !distro_running || stats_thread {
            return;
        }
        let want = self.shared.want_stats.load(Ordering::Relaxed);
        let due = self.shared.memory.lock().map(|m| memory::now_secs() - m.last_sample >= memory::IDLE_SAMPLE_SECS).unwrap_or(false);
        if !(want || due) || self.shared.mem_running.swap(true, Ordering::SeqCst) {
            return;
        }
        let w = self.clone();
        thread::spawn(move || {
            w.memory_sample(if want { Some(&[][..]) } else { None });
            w.shared.mem_running.store(false, Ordering::SeqCst);
        });
    }

    /// Jeden vzorek. `rows` = statistiky kontejnerů (plný vzorek s rozpadem), None = jen
    /// /proc/meminfo a VmmemWSL do historie.
    pub(super) fn memory_sample(&self, rows: Option<&[StatRow]>) {
        let distro = self.cfg().distro;
        let script = if rows.is_some() { memory::SAMPLE_SCRIPT } else { memory::SAMPLE_SCRIPT_MIN };
        let Ok(out) = wsl::run(&distro, None, None, &["sh", "-c", script]) else { return };
        let Some((mi, engine)) = memory::parse_sample(&out) else { return };
        let vmmem = self.shared.mem_probe.lock().ok().and_then(|mut p| p.get());
        self.alerts_memory(super::alerts::mem_input(&mi, vmmem));
        let t = memory::now_secs();
        let now = rows.map(|rows| {
            let ram = memory::total_ram();
            let limit = memory::limit_from(&crate::wslconfig::read(), ram);
            // paměť po kontejnerech ukazuje tabulka výkonu – tady jen součet
            let sum = rows.iter().map(|r| r.mem).sum();
            MemNow { b: memory::breakdown(&mi, vmmem, limit, ram, sum, engine.unwrap_or(0)) }
        });
        // historie na disku: paměť a čítače eth0 VM (stejný výstup wsl, žádné další volání)
        let net = memory::parse_net(&out);
        crate::history::store().record(t, Sample { vmmem, used: Some(mi.used()), cache: Some(mi.cache()), net, ..Default::default() });
        if let Ok(mut m) = self.shared.memory.lock() {
            m.history.push(MemPoint::new(t, vmmem, mi.used(), mi.cache()));
            m.last_sample = t;
            if now.is_some() {
                m.now = now;
            }
        }
        self.ctx.request_repaint();
    }

    /// Zapíše výsledek uvolnění paměti do historie a pošle ho UI.
    pub(super) fn memory_freed(&self, auto: bool, r: Result<FreeEvent, String>) {
        if let (Ok(e), Ok(mut m)) = (&r, self.shared.memory.lock()) {
            m.history.add_event(*e);
            crate::history::store().record_event(*e);
        }
        self.send(Msg::MemoryFreed { auto, result: r });
        // nový bod do grafu hned po uvolnění
        self.memory_sample(None);
    }

    /// Historie na disku ze statistik kontejnerů: celkové CPU (% všech CPU WSL, stejně jako
    /// dlaždice na přehledu) a úložiště enginu z `/system/df`.
    pub(super) fn history_stats(&self, rows: &[StatRow], df: Option<&crate::docker::DfSummary>, ncpu: f64) {
        // fold od 0.0: `sum()` prázdného seznamu dává -0.0
        let cpu = rows.iter().fold(0.0, |a, r| a + r.cpu) / ncpu.max(1.0);
        let engine = df.map(|s| [s.images, s.containers, s.volumes, s.build_cache].iter().map(|v| (*v).max(0) as u64).sum());
        crate::history::store().record(memory::now_secs(), Sample { cpu: Some(cpu), engine, ..Default::default() });
    }

    /// Historie na disku z kontroly disku (jednou za 5 min): disk distribuce, případně úložiště enginu.
    pub(super) fn history_disk(&self, d: &super::alerts::DiskInput) {
        let disk = d.inside.as_ref().map(|r| (r.used, r.avail));
        if disk.is_some() || d.engine_used.is_some() {
            crate::history::store().record(memory::now_secs(), Sample { disk, engine: d.engine_used, ..Default::default() });
        }
    }
}
