use super::*;

/// Restricts a profile file to the owner on unix; a no-op elsewhere.
fn restrict(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Ok(())
    }
}

/// Copies `source` to `backup`, creating the backup owner-only (0600) on unix.
fn copy_private(source: &Path, backup: &Path) -> std::io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut input = fs::File::open(source)?;
    let mut output = options.open(backup)?;
    let copied = std::io::copy(&mut input, &mut output)
        .and_then(|_| output.sync_all())
        .and_then(|()| restrict(backup));
    if copied.is_err() {
        // Don't leave a partial copy that looks like a preserved recovery.
        let _ = fs::remove_file(backup);
    }
    copied
}

impl App {
    pub(super) fn persist(&mut self) {
        fn sync_cwd(p: &mut Pane, sessions: &HashMap<String, Session>) {
            match p {
                Pane::Terminal { id, cwd, .. } => {
                    if let Some(s) = sessions.get(id) {
                        *cwd = s.cwd.clone();
                    }
                }
                Pane::Split { first, second, .. } => {
                    sync_cwd(first, sessions);
                    sync_cwd(second, sessions);
                }
                _ => {}
            }
        }
        for w in &mut self.state.workspaces {
            for t in &mut w.tabs {
                sync_cwd(&mut t.layout, &self.terminals);
            }
        }
        // A drafts.json that could not be restored is copied aside before it is
        // overwritten. Until that succeeds only drafts.json is held back: the
        // workspace itself is always saved.
        if self.recovery_conflict {
            let source = self.profile.join("drafts.json");
            let stamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos();
            let backup = self.profile.join(format!("drafts-conflict-{stamp}.json"));
            match copy_private(&source, &backup) {
                Ok(()) => {
                    self.recovery_conflict = false;
                    self.message =
                        format!("Conflicting recovery preserved at {}", backup.display());
                }
                // Removed since startup (by the user, say): nothing left to preserve.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound && !source.exists() => {
                    self.recovery_conflict = false;
                }
                Err(e) => self.message = format!("Cannot preserve recovery: {e}"),
            }
        }

        // Serialise on the UI thread (cheap); the background writer does the file
        // write and fsync so a slow disk never stalls a frame.
        let mut batch = vec![];
        match self.state.to_bytes() {
            Ok(bytes) => batch.push((self.profile.join("workspace.json"), bytes)),
            Err(e) => {
                self.message = format!("Workspace was not saved: {e}");
                return;
            }
        }
        if !self.recovery_conflict {
            let drafts: Vec<_> = self
                .documents
                .values()
                .filter(|d| d.dirty())
                .map(Document::draft)
                .collect();
            match serde_json::to_vec_pretty(&drafts) {
                Ok(bytes) => batch.push((self.profile.join("drafts.json"), bytes)),
                Err(e) => self.message = format!("Recovery was not saved: {e}"),
            }
        }
        self.writer.submit(batch);
        self.last_save = Instant::now();
    }
    /// Finishes pending document saves, then persists and waits until the files are
    /// on disk (exit, window close and smoke checks).
    pub(super) fn persist_now(&mut self) {
        // Drafts reflect what the saves left dirty, so they are taken afterwards.
        self.flush_documents();
        self.persist();
        self.writer.flush();
        self.report_write_errors();
    }
    /// Surfaces failures from the background writer in the status line.
    pub(super) fn report_write_errors(&mut self) {
        for e in self.writer.take_errors() {
            eprintln!("caelum-rs: {e}");
            self.message = e;
        }
    }
    pub(super) fn restore_drafts(&mut self) {
        let file = self.profile.join("drafts.json");
        if !file.exists() {
            return;
        }
        let result =
            (|| -> Result<Vec<Draft>> { Ok(serde_json::from_slice(&fs::read(&file)?)?) })();
        match result {
            Ok(drafts) => {
                // Only paths already open in an editor pane of the loaded workspace may be
                // restored; a draft never makes the app open a new file.
                let mut editors: Vec<(String, PathBuf)> = vec![];
                for w in &self.state.workspaces {
                    for t in &w.tabs {
                        let mut leaves = vec![];
                        t.layout.leaves(&mut leaves);
                        for p in leaves {
                            if let Pane::Editor { id, path } = p {
                                editors.push((id.clone(), path.clone()));
                            }
                        }
                    }
                }
                let paths: Vec<PathBuf> = editors.iter().map(|(_, p)| p.clone()).collect();
                for draft in drafts {
                    match Document::restore(draft, &paths) {
                        Ok(doc) => {
                            for (id, path) in &editors {
                                if path.canonicalize().ok().as_ref() == Some(&doc.path) {
                                    self.documents.insert(id.clone(), doc.clone());
                                }
                            }
                        }
                        Err(e) => {
                            eprintln!("caelum-rs: {e}");
                            self.message = e.to_string();
                            // Keeps drafts.json: it is copied aside before the next save.
                            self.recovery_conflict = true;
                        }
                    }
                }
            }
            Err(e) => {
                self.message = format!(
                    "Recovery could not be read; preserved at {}: {e}",
                    file.display()
                );
                self.recovery_conflict = true;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_app(profile: &Path) -> App {
        let mut app = App::new(
            &egui::Context::default(),
            profile.to_path_buf(),
            State::default(),
            String::new(),
            None,
            profile.join("config.toml"),
            SystemLook::default(),
            false,
        );
        app.test_shell = true;
        app
    }

    #[test]
    fn workspace_is_saved_while_a_recovery_conflict_cannot_be_preserved() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let mut app = test_app(&profile);
        // drafts.json can't be copied aside (it is a directory here; on a real system
        // a permission error or a full disk). Every save used to return early, so the
        // workspace was never written again for the rest of the session.
        fs::create_dir_all(profile.join("drafts.json")).unwrap();
        app.recovery_conflict = true;
        app.persist();
        app.writer.flush();
        assert!(profile.join("workspace.json").is_file());
        assert!(app.recovery_conflict, "drafts.json must still be held back");
        assert!(
            profile.join("drafts.json").is_dir(),
            "conflict was overwritten"
        );
        let leftovers: Vec<_> = fs::read_dir(&profile)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| {
                e.file_name()
                    .to_string_lossy()
                    .starts_with("drafts-conflict-")
            })
            .collect();
        assert!(leftovers.is_empty(), "partial backup left behind");
        app.state.workspaces[0].name = "Renamed".into();
        app.persist();
        app.writer.flush();
        let saved = fs::read_to_string(profile.join("workspace.json")).unwrap();
        assert!(saved.contains("Renamed"));
    }
    #[test]
    fn a_removed_recovery_conflict_stops_holding_back_drafts() {
        let dir = tempfile::tempdir().unwrap();
        let profile = dir.path().join("profile");
        let mut app = test_app(&profile);
        // The conflicting drafts.json was deleted after startup.
        app.recovery_conflict = true;
        app.persist();
        app.writer.flush();
        assert!(!app.recovery_conflict);
        assert!(profile.join("workspace.json").is_file());
        assert!(profile.join("drafts.json").is_file());
    }
}
