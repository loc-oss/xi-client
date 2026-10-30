use crate::types::{Config, Line, Operation, OperationType, Update, ViewId};

#[derive(Debug, Clone)]
pub struct StyleSpan {
    pub start: usize,
    pub len: usize,
    pub style_id: u64,
}

#[derive(Debug, Clone, Default)]
pub struct LineState {
    pub text: String,
    pub styles: Vec<StyleSpan>,
    pub cursors: Vec<usize>,
}

#[derive(Debug, Clone, Default)]
pub struct DocumentSnapshot {
    pub view_id: Option<ViewId>,
    pub lines: Vec<LineState>,
    pub selections: Vec<(usize, usize, usize, usize)>, // [line, col, line, col]
    pub pristine: bool,
    pub path: Option<std::path::PathBuf>,
    pub language: String,
    pub config: Config,
}

#[derive(Debug, Default)]
pub struct DocumentState {
    pub lines: Vec<Line>,
    pub pristine: bool,
    pub path: Option<std::path::PathBuf>,
    pub language: String,
    pub config: Config,
    pub selections: Vec<(usize, usize, usize, usize)>,
}

impl DocumentState {
    pub fn new() -> Self { Self::default() }

    pub fn apply(&mut self, update: &Update) {
        apply_ops(&mut self.lines, &update.operations);
        self.pristine = update.pristine;

        self.selections.clear();
        for ann in &update.annotations {
            if ann.ann_type == "selection" {
                for r in &ann.ranges {
                    if r.len() >= 4 {
                        self.selections.push((r[0] as usize, r[1] as usize, r[2] as usize, r[3] as usize));
                    }
                }
            }
        }
    }

    pub fn snapshot(&self, view_id: ViewId) -> DocumentSnapshot {
        let lines = self.lines.iter().map(|l| LineState {
            text: l.text.as_deref().unwrap_or("").trim_end_matches('\n').to_string(),
            styles: l.styles.iter().map(|s| StyleSpan {
                start: s.offset.max(0) as usize,
                len: s.length as usize,
                style_id: s.style_id,
            }).collect(),
            cursors: l.cursor.iter().map(|&c| c as usize).collect(),
        }).collect();

        DocumentSnapshot {
            view_id: Some(view_id),
            lines,
            selections: self.selections.clone(),
            pristine: self.pristine,
            path: self.path.clone(),
            language: self.language.clone(),
            config: self.config.clone(),
        }
    }
}

pub fn apply_ops(lines: &mut Vec<Line>, ops: &[Operation]) {
    let mut new_lines = Vec::new();
    let mut old_ix = 0usize;

    for op in ops {
        match op.operation_type {
            OperationType::Copy_ => {
                for _ in 0..op.nb_lines {
                    if old_ix < lines.len() {
                        new_lines.push(lines[old_ix].clone());
                    } else {
                        new_lines.push(Line::default());
                    }
                    old_ix += 1;
                }
            }
            OperationType::Skip => {
                old_ix += op.nb_lines as usize;
            }
            OperationType::Invalidate => {
                for _ in 0..op.nb_lines {
                    new_lines.push(Line::default());
                }
            }
            OperationType::Insert => {
                for l in &op.lines { new_lines.push(l.clone()); }
            }
            OperationType::Update => {
                for (i, l) in op.lines.iter().enumerate() {
                    let target = old_ix + i;
                    let mut nl = l.clone();
                    if nl.text.is_none() && target < lines.len() {
                        nl.text = lines[target].text.clone();
                    }
                    new_lines.push(nl);
                }
                old_ix += op.nb_lines as usize;
            }
        }
    }
    *lines = new_lines;
}
