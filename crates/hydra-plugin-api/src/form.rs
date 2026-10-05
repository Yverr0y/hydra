//! The declarative form shared by prompts and settings.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Bool(bool),
    Number(f64),
    Text(String),
}

impl Value {
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(b) => Some(*b),
            _ => None,
        }
    }
}

pub type Answers = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldKind {
    #[serde(alias = "textbox")]
    Text,
    Secret,
    Number,
    #[serde(alias = "boolean")]
    Bool,
    Checkbox,
    #[serde(alias = "dropdown")]
    Choice,
    Radio,
    Path,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    pub key: String,
    pub label: String,
    #[serde(rename = "type")]
    pub kind: FieldKind,
    #[serde(default)]
    pub default: Option<Value>,
    #[serde(default)]
    pub help: Option<String>,
    #[serde(default)]
    pub options: Vec<String>,
}

impl Field {
    /// Checks a value against the field's declared type.
    ///
    /// # Errors
    /// Returns the reason the value is unacceptable.
    pub fn validate(&self, value: &Value) -> Result<(), String> {
        match (self.kind, value) {
            (FieldKind::Bool | FieldKind::Checkbox, Value::Bool(_)) => Ok(()),
            (FieldKind::Number, Value::Number(n)) if n.is_finite() => Ok(()),
            (FieldKind::Choice | FieldKind::Radio, Value::Text(t)) => {
                if self.options.iter().any(|o| o == t) {
                    Ok(())
                } else {
                    Err(format!(
                        "`{t}` is not one of the options for `{}`",
                        self.key
                    ))
                }
            }
            (FieldKind::Text | FieldKind::Secret | FieldKind::Path, Value::Text(_)) => Ok(()),
            _ => Err(format!("wrong type of value for `{}`", self.key)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Form {
    #[serde(default)]
    pub title: Option<String>,
    pub fields: Vec<Field>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(kind: FieldKind, options: &[&str]) -> Field {
        Field {
            key: "k".into(),
            label: "K".into(),
            kind,
            default: None,
            help: None,
            options: options.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn validates_each_kind_against_its_value_type() {
        let t = Value::Text("x".into());
        let b = Value::Bool(true);
        let n = Value::Number(3.0);
        assert!(field(FieldKind::Text, &[]).validate(&t).is_ok());
        assert!(field(FieldKind::Secret, &[]).validate(&t).is_ok());
        assert!(field(FieldKind::Path, &[]).validate(&t).is_ok());
        assert!(field(FieldKind::Bool, &[]).validate(&b).is_ok());
        assert!(field(FieldKind::Number, &[]).validate(&n).is_ok());
        assert!(field(FieldKind::Bool, &[]).validate(&t).is_err());
        assert!(field(FieldKind::Number, &[]).validate(&t).is_err());
        assert!(field(FieldKind::Text, &[]).validate(&b).is_err());
    }

    #[test]
    fn parses_and_validates_all_gui_setting_types() {
        for (name, value) in [
            ("textbox", Value::Text("sample".into())),
            ("dropdown", Value::Text("sample".into())),
            ("number", Value::Number(1.5)),
            ("radio", Value::Text("sample".into())),
            ("checkbox", Value::Bool(true)),
            ("boolean", Value::Bool(false)),
        ] {
            let field: Field = serde_json::from_value(serde_json::json!({
                "key":"setting", "label":"Setting", "type":name, "options":["sample"]
            }))
            .unwrap();
            field.validate(&value).unwrap();
            assert!(field.validate(&Value::Text("unknown".into())).is_err() || name == "textbox");
        }
    }

    #[test]
    fn rejects_non_finite_numbers() {
        let f = field(FieldKind::Number, &[]);
        assert!(f.validate(&Value::Number(f64::NAN)).is_err());
        assert!(f.validate(&Value::Number(f64::INFINITY)).is_err());
    }

    #[test]
    fn choice_must_be_a_declared_option() {
        let f = field(FieldKind::Choice, &["a", "b"]);
        assert!(f.validate(&Value::Text("a".into())).is_ok());
        assert!(f.validate(&Value::Text("c".into())).is_err());
    }

    #[test]
    fn untagged_value_keeps_bool_number_text_apart() {
        let v: Vec<Value> = serde_json::from_str(r#"[true, 2.5, "s"]"#).unwrap();
        assert_eq!(
            v,
            [
                Value::Bool(true),
                Value::Number(2.5),
                Value::Text("s".into())
            ]
        );
    }
}
