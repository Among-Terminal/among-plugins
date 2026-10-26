//! modified after extraction from Among Terminal: scoped managed pane references,
//! without the old numeric adapter. original attribution is retained in PROVENANCE.md.

use crate::managed::{Scope, Target};
use crate::protocol::{terminals, Status};
use serde_json::{json, Value};

pub fn managed_status(scope: &Scope, context: &Value) -> Status {
    let Target::Pane { reference } = scope.target else {
        return render_path(None);
    };
    if scope.slot != "pane_footer" {
        return render_path(None);
    }
    let expected = reference.get().to_string();
    let path = terminals(context)
        .find(|terminal| terminal["pane"].as_str() == Some(expected.as_str()))
        .and_then(|terminal| terminal["cwd"].as_str());
    render_path(path)
}

fn render_path(path: Option<&str>) -> Status {
    let mut result = Status::empty(Some(1000));
    let Some(path) = path.filter(|path| !path.is_empty()) else {
        return result;
    };
    let text = path.chars().take(480).collect::<String>();
    result
        .fragments
        .push(json!({"text":text,"tooltip":path,"priority":10}));
    result
}
