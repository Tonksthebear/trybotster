use super::*;

impl Hub {
    pub(super) fn ice_candidate_preview(candidate: &str) -> String {
        const MAX: usize = 220;
        let single_line = candidate.replace('\n', " ").replace('\r', " ");
        let char_count = single_line.chars().count();
        if char_count <= MAX {
            return single_line;
        }
        let truncated: String = single_line.chars().take(MAX).collect();
        format!("{truncated}...<truncated,len={char_count}>")
    }
}
