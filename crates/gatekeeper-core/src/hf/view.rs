//! What the UI shows for a repository: its variants with fit indicators
//! (spec §15.2).
//!
//! Responsibility: combine a listing, the grouping and the GPU size into one
//! serialisable value.
//! Not responsible for fetching (`api`) or grouping rules (`gguf_files`).

use serde::Serialize;

use super::api::{RepoFile, RepoListing};
use super::gguf_files::{Fit, Variant, fit_for, group_files};

/// A variant plus how well it fits.
#[derive(Debug, Clone, Serialize)]
pub struct VariantView {
    /// The variant itself.
    #[serde(flatten)]
    pub variant: Variant,
    /// Fit against the monitored GPU.
    pub fit: Fit,
}

/// A repository ready to display.
#[derive(Debug, Clone, Serialize)]
pub struct RepoView {
    /// Commit of the listing.
    pub sha: Option<String>,
    /// Quantisation variants, smallest first.
    pub variants: Vec<VariantView>,
    /// Vision projectors offered alongside.
    pub mmproj: Vec<RepoFile>,
    /// Draft sidecar files offered alongside (spec §10.6).
    pub sidecars: Vec<RepoFile>,
    /// GPU memory the fit was judged against (MiB); `None` if unknown.
    pub total_vram_mib: Option<u64>,
}

/// Builds the view.
pub fn build_view(listing: &RepoListing, total_vram_mib: Option<u64>) -> RepoView {
    let grouped = group_files(&listing.files);
    RepoView {
        sha: listing.sha.clone(),
        variants: grouped
            .variants
            .into_iter()
            .map(|v| VariantView { fit: fit_for(v.total_bytes, total_vram_mib.unwrap_or(0)), variant: v })
            .collect(),
        mmproj: grouped.mmproj,
        sidecars: grouped.sidecars,
        total_vram_mib,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn variants_carry_a_fit_verdict_and_serialise_flat() {
        let gib = 1024 * 1024 * 1024;
        let listing = RepoListing {
            sha: Some("abc".into()),
            files: vec![
                RepoFile { path: "m-Q4_K_M.gguf".into(), size: 4 * gib, sha256: Some("h".into()) },
                RepoFile { path: "m-Q8_0.gguf".into(), size: 100 * gib, sha256: Some("h".into()) },
                RepoFile { path: "mmproj-f16.gguf".into(), size: gib, sha256: None },
            ],
        };
        let view = build_view(&listing, Some(24 * 1024));
        assert_eq!(view.variants[0].fit, Fit::FitsFully);
        assert_eq!(view.variants[1].fit, Fit::TooBig);
        assert_eq!(view.mmproj.len(), 1);
        let json = serde_json::to_value(&view).expect("json");
        assert_eq!(json["variants"][0]["quant"], "Q4_K_M", "variant fields are flattened next to `fit`");
        assert_eq!(json["variants"][0]["fit"], "fits_fully");
    }
}
