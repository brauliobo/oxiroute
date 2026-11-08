use std::sync::Arc;

use super::{AuthoredRevision, ResolvedConfigDocument};

/// The last resolved document, valid for exactly the disk bytes that produced it.
///
/// Resolution is a pure function of the authored bytes unless the source reads other files
/// (native references), so only documents without dependencies are retained.
#[derive(Default)]
pub(super) struct LoadCache {
    document: Option<Arc<ResolvedConfigDocument>>,
    #[cfg(test)]
    pub(super) resolutions: usize,
}

impl LoadCache {
    pub(super) fn get(&self, disk: &AuthoredRevision) -> Option<Arc<ResolvedConfigDocument>> {
        self.document
            .as_ref()
            .filter(|document| document.authored_revision == *disk)
            .cloned()
    }

    pub(super) fn store(
        &mut self,
        document: ResolvedConfigDocument,
    ) -> Arc<ResolvedConfigDocument> {
        #[cfg(test)]
        {
            self.resolutions += 1;
        }
        let document = Arc::new(document);
        self.document = document
            .dependencies
            .is_empty()
            .then(|| Arc::clone(&document));
        document
    }
}
