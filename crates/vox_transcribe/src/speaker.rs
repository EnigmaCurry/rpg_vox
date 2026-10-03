//! Live speaker labels: online clustering of per-utterance voice
//! embeddings. Cheap and approximate; a full offline diarization
//! ([`crate::diarize`]) can replace the labels once the audio is complete.

/// Label for the `i`th speaker: A … Z, then AA, AB, ….
pub fn label(i: usize) -> String {
    let mut n = i;
    let mut out = Vec::new();
    loop {
        out.push(b'A' + (n % 26) as u8);
        if n < 26 {
            break;
        }
        n = n / 26 - 1;
    }
    out.reverse();
    String::from_utf8(out).expect("ascii")
}

/// Inverse of [`label`]: "A" → 0, "AA" → 26. `None` for anything else.
pub fn index(label: &str) -> Option<usize> {
    if label.is_empty() || !label.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    let mut n = 0usize;
    for b in label.bytes() {
        n = n.checked_mul(26)?.checked_add((b - b'A') as usize + 1)?;
    }
    Some(n - 1)
}

#[derive(Debug, Clone)]
pub struct ClusterConfig {
    /// Cosine similarity to a speaker's centroid needed to join it.
    pub threshold: f32,
    /// Utterances shorter than this never open a new speaker; their
    /// embeddings are too noisy. They still join a close enough one.
    pub min_new_ms: u64,
    /// Never open more speakers than this (`--speakers N`); once full,
    /// every utterance goes to its closest speaker.
    pub max_speakers: Option<usize>,
}

impl Default for ClusterConfig {
    fn default() -> Self {
        Self {
            threshold: 0.7,
            min_new_ms: 1200,
            max_speakers: None,
        }
    }
}

/// Speakers heard so far, each a running sum of unit-length embeddings.
#[derive(Debug, Clone, Default)]
pub struct OnlineClusters {
    pub cfg: ClusterConfig,
    centroids: Vec<Vec<f32>>,
}

impl OnlineClusters {
    pub fn new(cfg: ClusterConfig) -> Self {
        Self {
            cfg,
            centroids: Vec::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.centroids.len()
    }

    pub fn is_empty(&self) -> bool {
        self.centroids.is_empty()
    }

    /// Assign an utterance of `duration_ms` with embedding `emb` to a
    /// speaker, opening a new one when nothing is close. Returns the
    /// speaker index, or `None` when it can't be told.
    pub fn assign(&mut self, emb: &[f32], duration_ms: u64) -> Option<usize> {
        let e = unit(emb)?;
        let best = self
            .centroids
            .iter()
            .enumerate()
            .filter_map(|(i, c)| Some((i, dot(&e, &unit(c)?))))
            .max_by(|a, b| a.1.total_cmp(&b.1));
        let full = self
            .cfg
            .max_speakers
            .is_some_and(|m| self.centroids.len() >= m.max(1));
        let long = duration_ms >= self.cfg.min_new_ms;
        tracing::debug!(
            ?best,
            duration_ms,
            speakers = self.centroids.len(),
            "speaker match"
        );
        let idx = match best {
            Some((i, sim)) if sim >= self.cfg.threshold || full => i,
            _ if long && !full => {
                self.centroids.push(vec![0.0; e.len()]);
                self.centroids.len() - 1
            }
            _ => return None,
        };
        // Only confident, long utterances move a centroid.
        let joins = self.centroids[idx].iter().all(|&x| x == 0.0)
            || (long && best.is_some_and(|(i, sim)| i == idx && sim >= self.cfg.threshold));
        if joins {
            for (c, x) in self.centroids[idx].iter_mut().zip(&e) {
                *c += x;
            }
        }
        Some(idx)
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn unit(v: &[f32]) -> Option<Vec<f32>> {
    let n = dot(v, v).sqrt();
    (n > 1e-6 && n.is_finite()).then(|| v.iter().map(|x| x / n).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels() {
        assert_eq!(label(0), "A");
        assert_eq!(label(25), "Z");
        assert_eq!(label(26), "AA");
        assert_eq!(label(27), "AB");
        for i in [0, 1, 25, 26, 27, 700, 703] {
            assert_eq!(index(&label(i)), Some(i));
        }
        assert_eq!(index("a"), None);
        assert_eq!(index(""), None);
    }

    #[test]
    fn clusters_by_similarity() {
        let mut c = OnlineClusters::new(ClusterConfig::default());
        assert_eq!(c.assign(&[1.0, 0.0, 0.0], 2000), Some(0));
        assert_eq!(c.assign(&[0.9, 0.1, 0.0], 2000), Some(0));
        assert_eq!(c.assign(&[0.0, 1.0, 0.0], 2000), Some(1));
        // Short and unlike anyone: can't tell.
        assert_eq!(c.assign(&[0.0, 0.0, 1.0], 300), None);
        // Short but close: joins.
        assert_eq!(c.assign(&[0.1, 0.9, 0.0], 300), Some(1));
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn max_speakers_forces_closest() {
        let mut c = OnlineClusters::new(ClusterConfig {
            max_speakers: Some(1),
            ..Default::default()
        });
        assert_eq!(c.assign(&[1.0, 0.0], 2000), Some(0));
        assert_eq!(c.assign(&[0.0, 1.0], 2000), Some(0));
        assert_eq!(c.len(), 1);
    }
}
