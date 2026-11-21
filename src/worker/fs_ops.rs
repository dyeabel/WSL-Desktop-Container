//! Operace se soubory v distribuci (stránka Soubory).

use super::{FsOp, Msg, Worker};

impl Worker {
    pub(super) fn fs_op(&self, root: bool, op: FsOp) -> Result<String, String> {
        let host = self.host();
        let host = host.as_ref();
        use crate::fs;
        let changed = |r: Result<String, String>, ok: String| {
            self.send(Msg::FsChanged);
            r.map(|_| ok)
        };
        match op {
            FsOp::List(p) => {
                match fs::list(host, root, &p) {
                    Ok((cwd, entries)) => self.send(Msg::FsListing(cwd, entries)),
                    Err(e) => self.send(Msg::FsListError(p, e)),
                }
                Ok(String::new())
            }
            FsOp::Mkdir(p) => changed(fs::mkdir(host, root, &p), t!("Složka vytvořena", "Folder created").into()),
            FsOp::Touch(p) => changed(fs::touch(host, root, &p), t!("Soubor vytvořen", "File created").into()),
            FsOp::Rename(a, b) => changed(fs::rename(host, root, &a, &b), t!("Přejmenováno", "Renamed").into()),
            FsOp::Delete(p) => {
                let n = p.len();
                changed(fs::delete(host, root, &p), tf!("Smazáno položek: {n}", "Items deleted: {n}"))
            }
            FsOp::Chmod(m, p) => changed(fs::chmod(host, root, &m, &p), tf!("Práva nastavena na {m}", "Permissions set to {m}")),
            FsOp::Upload(win, dir) => {
                let mut src = Vec::new();
                for w in &win {
                    src.push(host.to_guest_path(w)?);
                }
                let n = src.len();
                changed(fs::copy(host, root, &src, &dir), tf!("Nahráno položek: {n}", "Items uploaded: {n}"))
            }
            FsOp::Download(items, win_dir) => {
                let dst = host.to_guest_path(&win_dir)?;
                let n = items.len();
                fs::copy(host, root, &items, &dst).map(|_| tf!("Staženo {n} položek do {win_dir}", "Downloaded {n} items to {win_dir}"))
            }
            FsOp::Read(p) => {
                let t = fs::read_text(host, root, &p)?;
                self.send(Msg::FsFile(p, t));
                Ok(String::new())
            }
            FsOp::Write(p, t) => changed(fs::write_text(host, root, &p, &t), tf!("Uloženo: {p}", "Saved: {p}")),
        }
    }

}
