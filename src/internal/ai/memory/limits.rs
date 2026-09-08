/// Frozen safety limits for one Episode source resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct EpisodeSourceLimits {
    pub(crate) max_objects: usize,
    pub(crate) max_candidate_objects: usize,
    pub(crate) max_tree_bytes: u64,
    pub(crate) max_object_bytes: u64,
    pub(crate) max_total_bytes: usize,
    pub(crate) max_context_fragments: usize,
    pub(crate) max_token_estimate: usize,
    pub(crate) max_ancestry_commits: usize,
}

impl EpisodeSourceLimits {
    pub(crate) const fn repo_v1() -> Self {
        Self {
            max_objects: 256,
            max_candidate_objects: 4096,
            max_tree_bytes: 4 * 1024 * 1024,
            max_object_bytes: 128 * 1024,
            max_total_bytes: 2 * 1024 * 1024,
            max_context_fragments: 64,
            max_token_estimate: 512 * 1024,
            max_ancestry_commits: 2048,
        }
    }

    pub(crate) fn validate(self) -> Result<Self, &'static str> {
        if self.max_objects == 0
            || self.max_candidate_objects < self.max_objects
            || self.max_tree_bytes == 0
            || self.max_object_bytes == 0
            || self.max_total_bytes == 0
            || self.max_context_fragments == 0
            || self.max_token_estimate == 0
            || self.max_ancestry_commits == 0
        {
            return Err("Episode source limits are invalid");
        }
        Ok(self)
    }
}

impl Default for EpisodeSourceLimits {
    fn default() -> Self {
        Self::repo_v1()
    }
}
