use crate::rating::{LibraryScope, RatingError};
use crate::server::types::VirtualFolder;
use crate::util::location_leaf;

/// Pure library/location scoping logic. Testable without a server.
///
/// Returns a `LibraryScope` with:
/// - `parent_id`: the ItemId of the matched library (for prefetch ParentId filter)
/// - `location_path`: the full server-side path for post-prefetch filtering
/// - `library_name`: the resolved library name (for force_rating lookup)
pub fn resolve_from_libraries(
    libraries: &[VirtualFolder],
    library_name: Option<&str>,
    location_name: Option<&str>,
) -> Result<LibraryScope, RatingError> {
    if library_name.is_none() && location_name.is_none() {
        return Ok(LibraryScope {
            parent_id: None,
            location_path: None,
            library_name: None,
        });
    }

    if libraries.is_empty() {
        return Err(RatingError::NoMusicLibraries);
    }

    let (lib, matched_location_path) = if let Some(lib_name) = library_name {
        // Find library by name (case-insensitive)
        let lib = libraries
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(lib_name))
            .ok_or_else(|| RatingError::LibraryNotFound {
                name: lib_name.to_string(),
                available: libraries.iter().map(|l| l.name.clone()).collect(),
            })?;

        // If location also specified, find it within this library
        let loc_path = if let Some(loc_name) = location_name {
            let path = lib
                .locations
                .iter()
                .find(|p| location_leaf(p).eq_ignore_ascii_case(loc_name))
                .ok_or_else(|| RatingError::LocationNotFound {
                    name: loc_name.to_string(),
                    available: lib
                        .locations
                        .iter()
                        .map(|p| location_leaf(p).to_string())
                        .collect(),
                })?;
            Some(path.clone())
        } else {
            None
        };

        (lib, loc_path)
    } else {
        // --location without --library: search all libraries
        let loc_name = location_name.unwrap();
        let mut found_lib = None;
        let mut found_path = None;
        for lib in libraries {
            for path in &lib.locations {
                if location_leaf(path).eq_ignore_ascii_case(loc_name) {
                    found_lib = Some(lib);
                    found_path = Some(path.clone());
                    break;
                }
            }
            if found_lib.is_some() {
                break;
            }
        }
        match found_lib {
            Some(lib) => (lib, found_path),
            None => {
                let all_locs: Vec<String> = libraries
                    .iter()
                    .flat_map(|l| l.locations.iter().map(|p| location_leaf(p).to_string()))
                    .collect();
                return Err(RatingError::LocationNotFound {
                    name: loc_name.to_string(),
                    available: all_locs,
                });
            }
        }
    };

    if lib.item_id.is_empty() {
        return Err(RatingError::MissingLibraryId(lib.name.clone()));
    }

    log::info!(
        "scoping to library '{}' (ID: {}){}",
        lib.name,
        lib.item_id,
        matched_location_path
            .as_ref()
            .map(|p| format!(", location '{}'", location_leaf(p)))
            .unwrap_or_default()
    );

    Ok(LibraryScope {
        parent_id: Some(lib.item_id.clone()),
        location_path: matched_location_path,
        library_name: Some(lib.name.clone()),
    })
}

/// Post-prefetch filter: keep only items whose path starts with the location path.
pub fn filter_by_location(
    items: Vec<(crate::server::types::AudioItemView, serde_json::Value)>,
    location_path: &str,
) -> Vec<(crate::server::types::AudioItemView, serde_json::Value)> {
    let prefix = normalize_path(location_path.trim_end_matches(['/', '\\']));
    let prefix_with_sep = format!("{prefix}/");
    let before = items.len();
    // Capture representative item path roots before `into_iter` consumes `items`,
    // so an empty result can show what the real paths look like.
    let samples = sample_path_roots(&items);
    let filtered: Vec<_> = items
        .into_iter()
        .filter(|(view, _)| {
            view.path
                .as_deref()
                .map(|p| normalize_path(p).starts_with(&prefix_with_sep))
                .unwrap_or(false)
        })
        .collect();
    log::info!(
        "location filter: {} / {} items under {}",
        filtered.len(),
        before,
        location_path,
    );
    // A resolved location that filters a non-empty set down to zero almost always
    // means the item paths use a different mount view than the library location
    // (e.g. UNC `\\host\share` vs posix `/share`), so the prefix never matches.
    // Warn loudly rather than returning a silently successful empty run.
    if filtered.is_empty() && before > 0 {
        log::warn!(
            "location '{location_path}' matched 0 of {before} items \
             (prefix '{prefix_with_sep}'). Item paths likely use a different \
             mount view than the library location (e.g. UNC vs posix). \
             Sample item path roots: {}",
            if samples.is_empty() {
                "<none>".to_string()
            } else {
                samples.join(", ")
            }
        );
    }
    filtered
}

/// Normalize path separators to forward slash and lowercase for comparison.
fn normalize_path(path: &str) -> String {
    path.replace('\\', "/").to_lowercase()
}

/// Representative, de-duplicated leading path roots (first two normalized
/// segments) drawn from real item paths, capped at 5. Pure and side-effect
/// free so the diagnostic content is unit-testable without a log harness.
pub(crate) fn sample_path_roots(
    items: &[(crate::server::types::AudioItemView, serde_json::Value)],
) -> Vec<String> {
    let mut roots: Vec<String> = Vec::new();
    for (view, _) in items {
        let Some(path) = view.path.as_deref() else {
            continue;
        };
        let norm = normalize_path(path);
        // Preserve the leading root marker so the diagnostic distinguishes UNC
        // (`//host/share`) from POSIX (`/share/...`) - the whole point of the
        // warning. Stripping it would render both as `host/share`.
        let (leading, rest) = if let Some(stripped) = norm.strip_prefix("//") {
            ("//", stripped)
        } else if let Some(stripped) = norm.strip_prefix('/') {
            ("/", stripped)
        } else {
            ("", norm.as_str())
        };
        let root: String = rest
            .split('/')
            .filter(|s| !s.is_empty())
            .take(2)
            .collect::<Vec<_>>()
            .join("/");
        let root = if root.is_empty() {
            norm
        } else {
            format!("{leading}{root}")
        };
        if !roots.contains(&root) {
            roots.push(root);
            if roots.len() >= 5 {
                break;
            }
        }
    }
    roots
}

/// Look up force_rating from server config for the given library/location scope.
/// Returns the force_rating string if found, None otherwise.
///
/// Precedence: location force_rating > library force_rating > None.
pub fn lookup_force_rating<'a>(
    server_config: &'a crate::config::ServerConfig,
    library_name: Option<&str>,
    location_name: Option<&str>,
) -> Option<&'a str> {
    let lib_name = library_name?;
    let lib_config = server_config
        .libraries
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case(lib_name))
        .map(|(_, cfg)| cfg)?;

    // Check location-level first
    if let Some(loc_name) = location_name
        && let Some(loc_config) = lib_config
            .locations
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(loc_name))
            .map(|(_, cfg)| cfg)
        && let Some(ref rating) = loc_config.force_rating
    {
        return Some(rating.as_str());
    }

    // Fall back to library-level
    lib_config.force_rating.as_deref()
}
