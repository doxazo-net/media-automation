use crate::rating::{LibraryScope, RatingError};
use crate::server::types::VirtualFolder;
use crate::util::{location_leaf, normalize_path};

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

/// Bounded leaf-segment pattern (`/leaf/`) for a location, used as a fallback
/// matcher when item paths and the library location report different mount views
/// (e.g. UNC `\\host\Classical` vs posix `/share/Classical`), which share no
/// prefix. Wrapping the leaf in separators keeps it a whole path component, so
/// leaf `classical` matches `/classical/` but not `/classical_remix/`.
///
/// Returns `None` for a degenerate leaf (empty / separators only) to avoid a
/// pattern like `//` that would match unrelated paths.
fn leaf_segment(location_path: &str) -> Option<String> {
    let leaf = normalize_path(location_leaf(location_path));
    let leaf = leaf.trim_matches('/');
    if leaf.is_empty() {
        None
    } else {
        Some(format!("/{leaf}/"))
    }
}

/// Post-prefetch filter: keep only items under the location.
///
/// Primary match is a normalized path-prefix. When that drops a non-empty set to
/// zero -- the UNC-vs-posix mount-view mismatch (issue #216) -- it retries with a
/// bounded leaf-segment match. The retry needs no cross-library guard because the
/// items are already scoped to the single resolved library by the prefetch query.
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

    let prefix_hit = items.iter().any(|(view, _)| {
        view.path
            .as_deref()
            .map(|p| normalize_path(p).starts_with(&prefix_with_sep))
            .unwrap_or(false)
    });
    // Engage the leaf fallback only when the prefix matched nothing in a
    // non-empty set; `None` means use the primary prefix match.
    let leaf = if prefix_hit {
        None
    } else {
        leaf_segment(location_path)
    };

    let filtered: Vec<_> = items
        .into_iter()
        .filter(|(view, _)| {
            let Some(path) = view.path.as_deref() else {
                return false;
            };
            let norm = normalize_path(path);
            match &leaf {
                Some(seg) => norm.contains(seg),
                None => norm.starts_with(&prefix_with_sep),
            }
        })
        .collect();

    match &leaf {
        Some(seg) if !filtered.is_empty() => log::info!(
            "location filter: {} / {} items under {} via leaf-segment fallback '{seg}' \
             (item paths use a different mount view than the library location, e.g. UNC vs posix)",
            filtered.len(),
            before,
            location_path,
        ),
        _ => log::info!(
            "location filter: {} / {} items under {}",
            filtered.len(),
            before,
            location_path,
        ),
    }
    // Neither the prefix nor the leaf fallback matched a non-empty set: surface
    // it loudly rather than returning a silently successful empty run.
    if filtered.is_empty() && before > 0 {
        log::warn!(
            "location '{location_path}' matched 0 of {before} items \
             (prefix '{prefix_with_sep}', leaf fallback also empty). Item paths \
             likely use a different mount view than the library location \
             (e.g. UNC vs posix). Sample item path roots: {}",
            if samples.is_empty() {
                "<none>".to_string()
            } else {
                samples.join(", ")
            }
        );
    }
    filtered
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

// ── Per-item force-rating rules (issue #235) ────────────────────────

/// A resolved force-rating rule: an item whose normalized path falls under this
/// folder is forced to `rating`. Built from server config + discovered library
/// folders so a *full* run honors the same forces a `--location`/`--library`
/// scoped run would. Location-level rules take precedence over library-level.
#[derive(Debug, Clone, PartialEq)]
pub struct ForceRule {
    /// Normalized folder path (lowercased, forward-slash, no trailing slash).
    prefix: String,
    /// `prefix` with a trailing slash, precomputed so the hot per-item match loop
    /// (82k items x N rules) does no per-check allocation.
    prefix_with_sep: String,
    /// Bounded leaf segment (`/leaf/`) for the UNC-vs-posix mount-view fallback
    /// (issue #216), or `None` for a degenerate (empty) leaf.
    leaf: Option<String>,
    rating: String,
    /// True for a location-level rule (beats a library-level rule on the same item).
    is_location: bool,
}

/// Build force-rating rules from a server's configured library/location
/// `force_rating`s, resolving each configured name to its real folder path(s)
/// via the discovered `VirtualFolder` data. A library-level `force_rating`
/// applies to every folder of that library; a location-level one applies to the
/// folder whose leaf matches the configured location name.
pub fn build_force_rules(
    server_config: &crate::config::ServerConfig,
    libraries: &[VirtualFolder],
) -> Vec<ForceRule> {
    let mut rules = Vec::new();
    for (lib_name, lib_cfg) in &server_config.libraries {
        let lib_has_force = lib_cfg.force_rating.is_some()
            || lib_cfg.locations.values().any(|l| l.force_rating.is_some());
        let Some(vf) = libraries
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(lib_name))
        else {
            // Fail loud, not silent: an unresolvable library that carries a force
            // is the exact #235 silent-ignore symptom (a typo or a server rename).
            if lib_has_force {
                log::warn!(
                    "configured library '{lib_name}' has a force_rating but no matching \
                     music library was found on the server; its force(s) are ignored"
                );
            }
            continue;
        };
        // Library-level force applies to every folder of the library.
        if let Some(rating) = &lib_cfg.force_rating {
            for loc_path in &vf.locations {
                rules.push(make_force_rule(loc_path, rating, false));
            }
        }
        // Location-level force applies to the folder whose leaf matches the name.
        for (loc_name, loc_cfg) in &lib_cfg.locations {
            let Some(rating) = &loc_cfg.force_rating else {
                continue;
            };
            match vf
                .locations
                .iter()
                .find(|p| location_leaf(p).eq_ignore_ascii_case(loc_name))
            {
                Some(loc_path) => rules.push(make_force_rule(loc_path, rating, true)),
                None => log::warn!(
                    "configured location '{loc_name}' (library '{lib_name}') has a \
                     force_rating but no matching folder in the library; its force is ignored"
                ),
            }
        }
    }
    rules
}

fn make_force_rule(location_path: &str, rating: &str, is_location: bool) -> ForceRule {
    let prefix = normalize_path(location_path.trim_end_matches(['/', '\\']));
    let prefix_with_sep = format!("{prefix}/");
    ForceRule {
        prefix,
        prefix_with_sep,
        leaf: leaf_segment(location_path),
        rating: rating.to_string(),
        is_location,
    }
}

/// Resolve the effective forced rating for an item path. Matching mirrors
/// `filter_by_location`'s two-phase, set-level precedence: first choose among
/// **prefix** matches (aligned mount views); only when *no* rule prefix-matches
/// the item — a genuine mount-view mismatch (UNC vs posix, issue #216) — fall
/// back to bounded leaf-segment matches. Within each phase a location-level rule
/// beats a library-level one, ties broken by the longest (most specific) prefix.
///
/// Phasing (not a per-rule OR) is what stops a location leaf like `/classical/`
/// from out-ranking a real library prefix match on an unrelated `.../Classical/`
/// folder in an aligned-mount run.
pub fn resolve_force_rating<'a>(
    rules: &'a [ForceRule],
    item_path: Option<&str>,
) -> Option<&'a str> {
    let norm = normalize_path(item_path?);
    let best = rules
        .iter()
        .filter(|rule| force_rule_prefix_matches(rule, &norm))
        .reduce(better_force_rule)
        .or_else(|| {
            rules
                .iter()
                .filter(|rule| force_rule_leaf_matches(rule, &norm))
                .reduce(better_force_rule)
        });
    best.map(|r| r.rating.as_str())
}

fn force_rule_prefix_matches(rule: &ForceRule, norm_path: &str) -> bool {
    !rule.prefix.is_empty() && norm_path.starts_with(&rule.prefix_with_sep)
}

fn force_rule_leaf_matches(rule: &ForceRule, norm_path: &str) -> bool {
    rule.leaf
        .as_deref()
        .is_some_and(|seg| norm_path.contains(seg))
}

fn better_force_rule<'a>(a: &'a ForceRule, b: &'a ForceRule) -> &'a ForceRule {
    match (a.is_location, b.is_location) {
        (true, false) => a,
        (false, true) => b,
        _ if b.prefix.len() > a.prefix.len() => b,
        _ => a,
    }
}

// ── Per-song overrides (issue #236) ─────────────────────────────────

/// Resolve the most specific per-song override for an item path: the override
/// whose normalized match-key is a substring of the normalized item path, with
/// the longest key winning when several match.
pub fn resolve_override<'a>(
    overrides: &'a [crate::config::OverrideRule],
    item_path: Option<&str>,
) -> Option<&'a crate::config::OverrideRule> {
    let norm = normalize_path(item_path?);
    overrides
        .iter()
        .filter(|o| !o.match_key.is_empty() && norm.contains(&o.match_key))
        .max_by_key(|o| o.match_key.len())
}

/// True when a (already-normalized) match-key appears in the item's normalized
/// path. Used for the per-override match-count diagnostic (issue #236),
/// independent of precedence so a shadowed or over-broad key reports its true
/// reach.
pub fn path_contains_key(item_path: Option<&str>, normalized_key: &str) -> bool {
    item_path
        .map(|p| normalize_path(p).contains(normalized_key))
        .unwrap_or(false)
}
