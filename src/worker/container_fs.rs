//! Soubory v kontejneru (prohlížeč souborů kontejneru) – výpis, editor, nahrání, stažení.

use super::{Msg, Worker};
use crate::container_fs as cfs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub enum CfOp {
    List(String),
    Read(String),
    /// (cesta, text)
    Write(String, String),
    Mkdir(String),
    Delete(Vec<String>),
    /// (z, do)
    Rename(String, String),
    /// Windows soubory / složky -> adresář v kontejneru
    Upload(Vec<String>, String),
    /// položky v kontejneru -> Windows složka
    Download(Vec<String>, String),
}

impl CfOp {
    /// Krátká operace (pool vláken); nahrání, stažení a mazání běží ve vlastním vlákně.
    pub fn is_quick(&self) -> bool {
        matches!(self, CfOp::List(_) | CfOp::Read(_) | CfOp::Mkdir(_) | CfOp::Rename(..) | CfOp::Write(..))
    }
}

/// Zprávy pro konkrétní prohlížeč (podle čísla pohledu).
pub enum CfMsg {
    /// (požadovaná cesta, výsledek)
    Listing(String, Result<cfs::Listing, String>),
    /// (skutečná cesta po dohledání symlinku, text)
    File(String, String),
    /// obsah se změnil – načíst znovu
    Changed,
}

impl Worker {
    pub(super) fn container_fs(&self, view: u64, id: String, op: CfOp) -> Result<String, String> {
        let d = self.docker();
        let send = |m: CfMsg| self.send(Msg::ContainerFs(view, m));
        let changed = |r: Result<(), String>, ok: String| {
            send(CfMsg::Changed);
            r.map(|_| ok)
        };
        match op {
            CfOp::List(p) => {
                let r = cfs::list(&d, &id, &p);
                send(CfMsg::Listing(p, r));
                Ok(String::new())
            }
            CfOp::Read(p) => {
                let (path, text) = cfs::read_text(&d, &id, &p)?;
                send(CfMsg::File(path, text));
                Ok(String::new())
            }
            CfOp::Write(p, text) => changed(cfs::write_text(&d, &id, &p, &text), tf!("Uloženo: {p}", "Saved: {p}")),
            CfOp::Mkdir(p) => changed(cfs::mkdir(&d, &id, &p), t!("Složka vytvořena", "Folder created").into()),
            CfOp::Delete(p) => {
                let n = p.len();
                changed(cfs::delete(&d, &id, &p), tf!("Smazáno položek: {n}", "Items deleted: {n}"))
            }
            CfOp::Rename(a, b) => changed(cfs::rename(&d, &id, &a, &b), t!("Přejmenováno", "Renamed").into()),
            CfOp::Upload(win, dir) => {
                let items: Vec<PathBuf> = win.iter().map(PathBuf::from).collect();
                self.send(Msg::Toast(true, tf!("Nahrávám {} položek do {dir}…", "Uploading {} items to {dir}…", items.len())));
                // průběh nejvýš jednou za 3 s
                let mut last = Instant::now();
                let progress = |done: u64, total: u64| {
                    if last.elapsed() >= Duration::from_secs(3) && total > 0 && done < total {
                        last = Instant::now();
                        let pct = done * 100 / total;
                        self.send(Msg::Toast(true, tf!("Nahrávání: {pct} % ({} z {})", "Uploading: {pct} % ({} of {})", crate::ui::fmt_bytes(done), crate::ui::fmt_bytes(total))));
                    }
                };
                let r = cfs::upload(&d, &id, &items, &dir, progress);
                send(CfMsg::Changed);
                let (files, size) = r?;
                Ok(tf!("Nahráno do {dir}: {files} souborů ({})", "Uploaded to {dir}: {files} files ({})", crate::ui::fmt_bytes(size)))
            }
            CfOp::Download(items, win_dir) => {
                self.send(Msg::Toast(true, tf!("Stahuji {} položek…", "Downloading {} items…", items.len())));
                let rep = cfs::download(&d, &id, &items, std::path::Path::new(&win_dir))?;
                let mut msg = tf!(
                    "Staženo do {win_dir}: {} souborů, {} složek ({})",
                    "Downloaded to {win_dir}: {} files, {} folders ({})",
                    rep.files,
                    rep.dirs,
                    crate::ui::fmt_bytes(rep.bytes)
                );
                if !rep.skipped.is_empty() {
                    let first = rep.skipped.iter().take(3).cloned().collect::<Vec<_>>().join("; ");
                    msg.push_str(&tf!("\nPřeskočeno {}: {first}{}", "\nSkipped {}: {first}{}", rep.skipped.len(), if rep.skipped.len() > 3 { "; …" } else { "" }));
                }
                Ok(msg)
            }
        }
    }
}
