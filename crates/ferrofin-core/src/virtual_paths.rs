//! The two virtual-path tokens Jellyfin persists in place of the server's own
//! directories, and their expansion on read.
//!
//! `XmlSerializer` documents, `BaseItems.Path` and `BaseItemImageInfos.Path`
//! all go through `IApplicationHost.ReverseVirtualPath` on the way in and
//! `ExpandVirtualPath` on the way out (`BaseItemRepository.Map`), so a stored
//! image path reads `%MetadataPath%/library/ab/abcd…/poster.jpg`. That is what
//! makes a Jellyfin database portable between data directories — and what
//! makes an adopted one unreadable until the same expansion is applied on
//! Ferrofin's side. Ferrofin writes resolved paths, so expanding on read is
//! the identity for rows it wrote itself.

use ferrofin_traits::system::ServerApplicationPaths;

use crate::app_paths::FerrofinServerApplicationPaths;

/// Expands `%AppDataPath%` and `%MetadataPath%` to this server's directories.
///
/// Port of the read half of the virtual-path pair (`ExpandVirtualPath`). A
/// [`VirtualPathExpander::identity`] never rewrites, for callers (tests, mostly)
/// that have no paths and store none of the tokens.
#[derive(Debug, Clone, Default)]
pub struct VirtualPathExpander {
    /// The `%AppDataPath%` replacement, or empty for no substitution.
    data: String,
    /// The `%MetadataPath%` replacement, or empty for no substitution.
    metadata: String,
}

impl VirtualPathExpander {
    /// An expander for this server's directories.
    #[must_use]
    pub fn from_paths(paths: &dyn ServerApplicationPaths) -> Self {
        Self {
            data: paths.data_path(),
            metadata: paths.internal_metadata_path(),
        }
    }

    /// An expander that leaves every path alone.
    #[must_use]
    pub fn identity() -> Self {
        Self::default()
    }

    /// The stored path with both tokens replaced, case-insensitively, as
    /// `string.Replace(…, StringComparison.OrdinalIgnoreCase)` does upstream.
    #[must_use]
    pub fn expand(&self, path: &str) -> String {
        let data = replace_ignore_ascii_case(
            path,
            FerrofinServerApplicationPaths::VIRTUAL_DATA_PATH,
            &self.data,
        );
        replace_ignore_ascii_case(
            &data,
            FerrofinServerApplicationPaths::VIRTUAL_INTERNAL_METADATA_PATH,
            &self.metadata,
        )
    }

    /// [`Self::expand`] for an optional path.
    #[must_use]
    pub fn expand_opt(&self, path: Option<&str>) -> Option<String> {
        path.map(|p| self.expand(p))
    }
}

/// Case-insensitive `String.Replace` of every occurrence of `from` with `to`.
///
/// Mirrors C# `string.Replace(old, new, StringComparison.OrdinalIgnoreCase)`.
/// An empty `from` is a no-op (avoids an infinite loop); an empty `to` with a
/// non-empty `from` deletes the token, which is why the identity expander keeps
/// its tokens by never being asked — see [`VirtualPathExpander::expand`]'s
/// callers, which only construct one from real paths.
pub(crate) fn replace_ignore_ascii_case(haystack: &str, from: &str, to: &str) -> String {
    if from.is_empty() || to.is_empty() {
        return haystack.to_owned();
    }
    let lower_hay = haystack.to_ascii_lowercase();
    let lower_from = from.to_ascii_lowercase();
    let mut out = String::with_capacity(haystack.len());
    let mut cursor = 0;
    while let Some(rel) = lower_hay[cursor..].find(&lower_from) {
        let start = cursor + rel;
        out.push_str(&haystack[cursor..start]);
        out.push_str(to);
        cursor = start + from.len();
    }
    out.push_str(&haystack[cursor..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expander() -> VirtualPathExpander {
        VirtualPathExpander {
            data: "/var/lib/ferrofin/data".to_owned(),
            metadata: "/var/lib/ferrofin/data/metadata".to_owned(),
        }
    }

    #[test]
    fn expands_the_metadata_token_as_jellyfin_stores_image_paths() {
        assert_eq!(
            expander().expand("%MetadataPath%/library/ab/abcdef/poster.jpg"),
            "/var/lib/ferrofin/data/metadata/library/ab/abcdef/poster.jpg"
        );
    }

    #[test]
    fn expands_the_data_token_and_is_case_insensitive() {
        assert_eq!(
            expander().expand("%appdatapath%/collections/Favourites"),
            "/var/lib/ferrofin/data/collections/Favourites"
        );
    }

    #[test]
    fn resolved_paths_pass_through_unchanged() {
        let media = "/srv/fastmedia/shows/S01E01-thumb.jpg";
        assert_eq!(expander().expand(media), media);
        assert_eq!(expander().expand(""), "");
    }

    #[test]
    fn identity_keeps_the_tokens() {
        let stored = "%MetadataPath%/library/x.jpg";
        assert_eq!(VirtualPathExpander::identity().expand(stored), stored);
    }

    #[test]
    fn expand_opt_maps_through() {
        assert_eq!(expander().expand_opt(None), None);
        assert_eq!(
            expander().expand_opt(Some("%MetadataPath%/x")).as_deref(),
            Some("/var/lib/ferrofin/data/metadata/x")
        );
    }
}
