// `Store` methods that exist only for tests: unsecured wrappers around the
// `*_secured` operations and a few fixtures. Kept out of the production files.

use super::*;

impl Store {
    pub fn new(root: PathBuf) -> Result<Self, StoreError> {
        Self::with_options(
            root,
            "http://symbol".to_string(),
            Arc::new(SystemClock),
            DecayPolicy::default(),
        )
    }

    pub fn with_public_url(root: PathBuf, public_url: String) -> Result<Self, StoreError> {
        Self::with_expiry_defaults(root, public_url, DecayPolicy::default())
    }

    pub(super) fn with_clock(
        root: PathBuf,
        public_url: String,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        Self::with_options(root, public_url, clock, DecayPolicy::default())
    }

    pub fn list_files(&self, name: &str) -> Result<Vec<String>, StoreError> {
        let name = parse_site_name(name)?;
        let mut db = self.inner.readers.get();
        require_site_locked(&mut db, name)?;
        let site_id = site_id_locked(&mut db, name)?;
        let mut paths = files::table
            .filter(files::site_id.eq(site_id))
            .select(files::path)
            .order(files::path)
            .load::<String>(&mut *db)?;
        paths.extend(
            allocated_entries::table
                .filter(allocated_entries::site_id.eq(site_id))
                .select(allocated_entries::path)
                .load::<String>(&mut *db)?,
        );
        paths.sort_unstable();
        Ok(paths)
    }

    pub(super) fn set_before_content_commit(&self, hook: impl FnOnce() + Send + 'static) {
        *self.inner.before_content_commit.lock().unwrap() = Some(Box::new(hook));
    }

    pub(super) fn run_before_content_commit(&self) {
        let hook = self.inner.before_content_commit.lock().unwrap().take();
        if let Some(hook) = hook {
            hook();
        }
    }

    pub fn replace_site(
        &self,
        name: &str,
        bytes: &[u8],
        kind: Kind,
        filename: Option<&str>,
        unpack: bool,
    ) -> Result<usize, StoreError> {
        let name = parse_site_name(name)?.to_string();
        let tmp = self.tmp_dir(&name);
        if tmp.exists() {
            fs::remove_dir_all(&tmp)?;
        }
        fs::create_dir_all(&tmp)?;
        match write_payload(&tmp, bytes, kind, filename, unpack) {
            Ok(_) => {}
            Err(err) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(err.into());
            }
        }
        let staged = match stage_dir(&tmp) {
            Ok(files) if !files.is_empty() => files,
            Ok(_) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(UploadError::EmptyArchive.into());
            }
            Err(err) => {
                let _ = fs::remove_dir_all(&tmp);
                return Err(err.into());
            }
        };
        let n = staged.len();
        let result = self.merge_staged(&name, &staged, UndoKind::Put);
        let _ = fs::remove_dir_all(&tmp);
        result?;
        Ok(n)
    }

    pub fn put_file(&self, name: &str, rel: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let name = parse_site_name(name)?.to_string();
        let rel = normalize_nonempty_rel(rel)?;
        if is_junk(Path::new(&rel), Some(bytes)) {
            return Err(UploadError::Junk.into());
        }
        let staged = stage_bytes(&rel, bytes);
        self.upsert_file(&name, &staged).map(|_| ())
    }

    pub fn put_uploaded_file(
        &self,
        name: &str,
        rel: &str,
        source: PathBuf,
        expected_tree_hash: Option<&str>,
    ) -> Result<MutationResult, StoreError> {
        self.put_uploaded_file_secured(
            name,
            rel,
            source,
            PublishOptions {
                expected_tree_hash,
                ..PublishOptions::default()
            },
        )
    }

    pub fn splice_file(
        &self,
        name: &str,
        path: &str,
        base_hash: &str,
        splices: &[Splice<'_>],
        options: FileMutationOptions<'_>,
    ) -> Result<AllocatedFile, StoreError> {
        self.splice_file_with_limit(
            name,
            path,
            base_hash,
            splices,
            options,
            MAX_SPLICE_RESULT_SIZE,
        )
    }

    pub fn pop_site(&self, name: &str) -> Result<Vec<u8>, StoreError> {
        let name = parse_site_name(name)?;
        self.write(|tx| {
            let archive = site_files(tx, &self.inner.blob_files, name)?;
            let packed = pack_tar_gz(&archive.files)?;
            snapshot_site(tx, name, UndoKind::DeleteSite, self.now_millis())?;
            retain_management_tombstone(tx, name, self.now_millis())?;
            diesel::delete(sites::table.filter(sites::name.eq(name))).execute(&mut *tx)?;
            let removed = gc_blobs(tx, self.now_millis())?;
            Ok((packed, removed))
        })
    }

    pub fn pack_site(&self, name: &str, format: ArchiveFormat) -> Result<Vec<u8>, StoreError> {
        let name = parse_site_name(name)?;
        let mut db = self.inner.readers.get();
        let archive = site_files(&mut db, &self.inner.blob_files, name)?;
        drop(db);
        match format {
            ArchiveFormat::Tar => pack_tar(&archive.files),
            ArchiveFormat::TarGz => pack_tar_gz(&archive.files),
            ArchiveFormat::Zip => pack_zip(&archive.files),
        }
        .map_err(StoreError::Io)
    }

    pub fn copy_site(
        &self,
        source: &str,
        destination: Option<&str>,
        idempotency: Option<&Idempotency>,
    ) -> Result<(String, MutationResult), StoreError> {
        self.copy_site_secured(
            source,
            destination,
            idempotency,
            CreationSecurity::default(),
        )
    }

    pub fn move_site(
        &self,
        source: &str,
        destination: &str,
    ) -> Result<(String, MutationResult), StoreError> {
        self.move_site_secured(source, destination, None)
    }

    pub fn delete_file(&self, name: &str, rel: &str) -> Result<MutationResult, StoreError> {
        self.delete_file_secured(name, rel, None)
    }

    pub(super) fn upsert_file(
        &self,
        name: &str,
        file: &StagedFile,
    ) -> Result<MutationResult, StoreError> {
        reject_reserved_path(&file.path)?;
        self.merge_staged(name, std::slice::from_ref(file), UndoKind::Put)
    }

    pub fn put_alias(
        &self,
        name: &str,
        path: &str,
        target: &str,
        options: FileMutationOptions<'_>,
    ) -> Result<MutationResult, StoreError> {
        self.put_aliases(name, &[AliasSpec { path, target }], options)
    }

    pub fn put_aliases(
        &self,
        name: &str,
        specs: &[AliasSpec<'_>],
        options: FileMutationOptions<'_>,
    ) -> Result<MutationResult, StoreError> {
        self.put_aliases_with_receipt(name, specs, options)
            .map(|result| result.mutation)
    }

    pub fn aliases(&self, name: &str) -> Result<Vec<AliasEntry>, StoreError> {
        let name = parse_site_name(name)?;
        let mut db = self.inner.readers.get();
        let site_id = site_id_locked(&mut db, name)?;
        aliases::table
            .filter(aliases::site_id.eq(site_id))
            .select(AliasRow::as_select())
            .order(aliases::path)
            .load::<AliasRow>(&mut *db)?
            .into_iter()
            .map(alias_entry)
            .collect()
    }

    pub fn alias_inventory(&self, name: &str) -> Result<AliasInventory, StoreError> {
        let name = parse_site_name(name)?;
        let mut db = self.inner.readers.get();
        let mut snapshot = DbTransaction::begin(&mut db)?;
        let (site_id, content_revision, tree_hash) = sites::table
            .filter(sites::name.eq(name))
            .select((sites::id, sites::content_revision, sites::tree_hash))
            .first::<(i64, i64, TreeHash)>(&mut *snapshot)
            .map_err(map_sql)?;
        let aliases = aliases::table
            .filter(aliases::site_id.eq(site_id))
            .select(AliasRow::as_select())
            .order(aliases::path)
            .load::<AliasRow>(&mut *snapshot)?
            .into_iter()
            .map(alias_entry)
            .collect::<Result<Vec<_>, _>>()?;
        snapshot.commit()?;
        Ok(AliasInventory {
            site: name.to_string(),
            content_revision: content_revision.cast_unsigned(),
            tree_hash: tree_hash.to_wire(),
            aliases,
        })
    }

    pub fn alias_stats(&self, name: &str) -> Result<AliasStats, StoreError> {
        let inventory = self.alias_inventory(name)?;
        let aliases = u64::try_from(inventory.aliases.len()).expect("alias count fits in u64");
        let resolved = u64::try_from(
            inventory
                .aliases
                .iter()
                .filter(|alias| alias.resolved_kind.is_some())
                .count(),
        )
        .expect("resolved alias count fits in u64");
        Ok(AliasStats {
            aliases,
            resolved,
            dangling: aliases - resolved,
        })
    }

    pub fn allocate_bytes(
        &self,
        name: &str,
        bytes: &[u8],
        spec: AllocationSpec<'_>,
        options: FileMutationOptions<'_>,
    ) -> Result<AllocatedFile, StoreError> {
        self.allocate_source(name, AllocationSource::Bytes(bytes), spec, options)
    }

    pub fn replace_allocated(
        &self,
        name: &str,
        current_path: &str,
        source: AllocationSource<'_>,
        options: FileMutationOptions<'_>,
    ) -> Result<AllocatedFile, StoreError> {
        let current_path = normalize_rel(current_path)?;
        let staged = self.stage_allocation_source(source, "")?;
        let request_fingerprint = content_mutation_request_fingerprint(
            name,
            &current_path,
            staged.hash,
            None,
            options.expected_tree_hash,
            UndoKind::Replace,
        );
        if let Some(replay) = self.replay_content_request(name, options, &request_fingerprint)? {
            return Ok(replay);
        }
        let metadata = self.allocated_metadata(name, &current_path)?;
        let destination = relocated_destination(staged.hash, &current_path, &metadata)?;
        self.run_before_content_commit();
        self.commit_allocated(
            name,
            Some(&current_path),
            &destination,
            &staged,
            None,
            None,
            options,
            UndoKind::Replace,
        )
    }

    pub fn propose_allocation(
        &self,
        name: &str,
        source: AllocationSource<'_>,
        spec: PendingAllocationSpec<'_>,
        authorization: Option<&ManagementToken>,
    ) -> Result<PendingAllocation, StoreError> {
        self.propose_allocation_idempotent(
            name,
            source,
            spec,
            FileMutationOptions {
                authorization,
                ..FileMutationOptions::default()
            },
        )
    }

    pub fn finalize_allocation(
        &self,
        name: &str,
        token: &str,
        naming: AllocatedName<'_>,
        options: FileMutationOptions<'_>,
    ) -> Result<AllocatedFile, StoreError> {
        self.finalize_pending(
            name,
            token,
            None,
            PendingFinalName::Generated(naming),
            options,
        )
    }

    pub fn finalize_allocation_custom(
        &self,
        name: &str,
        token: &str,
        basename: &str,
        options: FileMutationOptions<'_>,
    ) -> Result<AllocatedFile, StoreError> {
        self.finalize_pending(
            name,
            token,
            None,
            PendingFinalName::Custom(basename),
            options,
        )
    }

    pub fn cancel_allocation(
        &self,
        name: &str,
        token: &str,
        authorization: Option<&ManagementToken>,
    ) -> Result<(), StoreError> {
        if self.cancel_allocation_in_folder(name, token, None, authorization)? {
            Ok(())
        } else {
            Err(StoreError::InvalidPendingAllocation)
        }
    }

    pub(super) fn cancel_allocation_in_folder(
        &self,
        name: &str,
        token: &str,
        folder: Option<&str>,
        authorization: Option<&ManagementToken>,
    ) -> Result<bool, StoreError> {
        let name = parse_site_name(name)?;
        let folder = folder.map(normalize_folder).transpose()?;
        let now = self.now_millis();
        self.write_outcome(|tx| {
            authorize_locked(tx, name, authorization)?;
            let site_id = site_id_locked(tx, name)?;
            let pending = pending_allocations::table
                .find(token)
                .select((
                    pending_allocations::site_id,
                    pending_allocations::folder,
                    pending_allocations::request_fingerprint,
                ))
                .first::<(i64, String, String)>(&mut *tx)
                .optional()?;
            let Some((stored_site_id, stored_folder, request_metadata)) = pending else {
                return Ok(TxOutcome::Rollback(false));
            };
            if stored_site_id != site_id {
                return Err(StoreError::InvalidPendingAllocation);
            }
            if folder
                .as_deref()
                .is_some_and(|expected| expected != stored_folder)
            {
                return Err(StoreError::InvalidPendingAllocation);
            }
            let request = parse_pending_request_metadata(&request_metadata)?;
            if !request.legacy_fingerprint
                && request.authorization_hash != authorization.map(authorization_fingerprint)
            {
                return Err(StoreError::Unauthorized);
            }
            diesel::delete(
                pending_allocations::table
                    .find(token)
                    .filter(pending_allocations::site_id.eq(site_id)),
            )
            .execute(&mut *tx)?;
            let removed = gc_blobs(tx, now)?;
            Ok(TxOutcome::Commit(true, removed))
        })
    }

    pub fn prune_pending_allocations(&self) -> Result<usize, StoreError> {
        let now = self.now_millis();
        self.write(|tx| {
            let removed_pending = prune_pending_locked(tx, now)?;
            let removed = gc_blobs(tx, now)?;
            Ok((removed_pending, removed))
        })
    }

    pub fn set_expiry(
        &self,
        name: &str,
        rel: &str,
        policy: Option<ExpiryPolicy>,
    ) -> Result<ExpiryMutation, StoreError> {
        self.set_expiry_secured(name, rel, policy, None)
    }

    pub fn undo(&self, name: &str, guard: Option<&str>) -> Result<UndoResult, StoreError> {
        self.undo_secured(name, guard, None)
    }

    pub(super) fn prune_undo_and_gc(&self) -> Result<(), StoreError> {
        let now = self.now_millis();
        self.write(|tx| {
            prune_undo_locked(tx, now)?;
            prune_idempotency_locked(tx, now)?;
            prune_pending_locked(tx, now)?;
            let removed = gc_blobs(tx, now)?;
            Ok(((), removed))
        })
    }
}
