//! The candidate query behind name search.
//!
//! SQL only narrows: it returns every file whose path contains each literal
//! fragment of the query. Matching names, deriving folders, ranking and
//! paging happen in `crate::search`. Every file below a matching folder
//! contains that folder's name in its path, so the candidates are enough to
//! count and size each matching folder exactly.

#[allow(clippy::wildcard_imports)] // see the note in the parent module
use super::*;

/// A file that may match a name search.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchCandidate {
    pub site: String,
    pub path: String,
    pub bytes: u64,
    pub modified: i64,
}

/// `%fragment%` for `LIKE … ESCAPE '\'`.
fn contains_pattern(fragment: &str) -> String {
    let mut pattern = String::with_capacity(fragment.len() + 2);
    pattern.push('%');
    for character in fragment.chars() {
        if matches!(character, '%' | '_' | '\\') {
            pattern.push('\\');
        }
        pattern.push(character);
    }
    pattern.push('%');
    pattern
}

/// A search scope directory as `dir/`, or empty for the whole site.
pub fn normalize_search_prefix(raw: &str) -> Result<String, StoreError> {
    let directory = normalize_rel(raw.trim_matches('/'))?;
    Ok(if directory.is_empty() {
        directory
    } else {
        format!("{directory}/")
    })
}

impl Store {
    /// Every site name, sorted.
    pub fn site_names(&self) -> Result<Vec<String>, StoreError> {
        let mut db = self.inner.readers.get();
        Ok(sites::table
            .select(sites::name)
            .order(sites::name)
            .load(&mut *db)?)
    }

    /// Files and allocated entries whose path contains every fragment,
    /// case-insensitively for ASCII, in `site` (or every site) and below
    /// `prefix` (a normalized directory ending in `/`, or empty).
    ///
    /// The generated `symbol.toml` is never a candidate.
    pub fn search_candidates(
        &self,
        site: Option<&str>,
        prefix: &str,
        fragments: &[String],
    ) -> Result<Vec<SearchCandidate>, StoreError> {
        let site = site.map(parse_site_name).transpose()?;
        // Paths below `dir/` sort between `dir/` and `dir0`, `0` being the
        // character after `/`.
        let prefix_end = prefix
            .strip_suffix('/')
            .map(|directory| format!("{directory}0"));
        let patterns: Vec<String> = fragments.iter().map(|f| contains_pattern(f)).collect();
        let mut db = self.inner.readers.get();
        let mut snapshot = DbTransaction::begin(&mut db)?;
        let site_id = site
            .map(|name| site_id_locked(&mut snapshot, name))
            .transpose()?;

        let mut query = files::table
            .inner_join(sites::table)
            .filter(files::path.ne(MANIFEST_PATH))
            .select((sites::name, files::path, files::size, files::modified))
            .into_boxed();
        if let Some(site_id) = site_id {
            query = query.filter(files::site_id.eq(site_id));
        }
        if let Some(end) = &prefix_end {
            query = query.filter(files::path.ge(prefix).and(files::path.lt(end)));
        }
        for pattern in &patterns {
            query = query.filter(files::path.like(pattern).escape('\\'));
        }
        let mut rows = query.load::<(String, String, i64, i64)>(&mut *snapshot)?;

        let mut allocated = allocated_entries::table
            .inner_join(sites::table)
            .select((
                sites::name,
                allocated_entries::path,
                allocated_entries::size,
                allocated_entries::modified,
            ))
            .into_boxed();
        if let Some(site_id) = site_id {
            allocated = allocated.filter(allocated_entries::site_id.eq(site_id));
        }
        if let Some(end) = &prefix_end {
            allocated = allocated.filter(
                allocated_entries::path
                    .ge(prefix)
                    .and(allocated_entries::path.lt(end)),
            );
        }
        for pattern in &patterns {
            allocated = allocated.filter(allocated_entries::path.like(pattern).escape('\\'));
        }
        rows.extend(allocated.load::<(String, String, i64, i64)>(&mut *snapshot)?);
        snapshot.commit()?;

        Ok(rows
            .into_iter()
            .map(|(site, path, size, modified)| SearchCandidate {
                site,
                path,
                bytes: size.cast_unsigned(),
                modified,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::contains_pattern;

    #[test]
    fn like_wildcards_in_fragments_are_literal() {
        assert_eq!(contains_pattern("kan-ext"), "%kan-ext%");
        assert_eq!(contains_pattern("100%_a\\b"), "%100\\%\\_a\\\\b%");
    }
}
