//! GraphQL SDL parser.
//!
//! Walks the CST produced by [`apollo_parser`] and emits the corresponding
//! [`ApiSpec`].
//!
//! The parser supports:
//!
//! * Object type definitions  → Rust struct
//! * Input object definitions → Rust struct
//! * Enum type definitions    → Rust enum
//! * Scalar type definitions  → Rust alias to `String`
//! * Fields on the `Query` and `Mutation` types → [`Operation`]s with
//!   [`Protocol::GraphQl`] and HTTP `POST /graphql`.
//!
//! Unions, interfaces, and directives are recognised but emit no IR; their
//! fields fall back to `serde_json::Value`.

use apollo_parser::cst::{
    self, CstNode, Definition, FieldDefinition, InputValueDefinition, Type as GqlType,
};
use apollo_parser::Parser;

use crate::error::{GenError, Result};
use crate::ir::{
    ApiKind, ApiSpec, EnumVariant, Field, HttpMethod, Operation, Param, ParamLocation, Protocol,
    StreamingMode, TypeDef,
};
use crate::parsers::naming::{crate_name, pascal_ident, snake_ident};

/// Parse a GraphQL schema string into an [`ApiSpec`].
pub fn parse(raw: &str) -> Result<ApiSpec> {
    let parser = Parser::new(raw);
    let cst = parser.parse();
    if !cst.errors().count().eq(&0) {
        let msg = cst
            .errors()
            .map(|e| e.message().to_string())
            .collect::<Vec<_>>()
            .join("; ");
        return Err(GenError::Parse {
            kind: "graphql",
            message: msg,
        });
    }

    let document = cst.document();
    let mut types: Vec<TypeDef> = Vec::new();
    let mut operations: Vec<Operation> = Vec::new();

    for def in document.definitions() {
        match def {
            Definition::ObjectTypeDefinition(obj) => {
                let name = node_name(obj.name());
                if name == "Query" || name == "Mutation" || name == "Subscription" {
                    let http_method = HttpMethod::Post;
                    let streaming = if name == "Subscription" {
                        StreamingMode::ServerStream
                    } else {
                        StreamingMode::Unary
                    };
                    if let Some(fd) = obj.fields_definition() {
                        for field in fd.field_definitions() {
                            operations.push(field_to_operation(&field, http_method, streaming));
                        }
                    }
                } else {
                    types.push(TypeDef::Struct {
                        name: pascal_ident(&name),
                        description: description_of(obj.description()),
                        fields: object_fields(obj.fields_definition()),
                    });
                }
            }
            Definition::InputObjectTypeDefinition(inp) => {
                let name = node_name(inp.name());
                types.push(TypeDef::Struct {
                    name: pascal_ident(&name),
                    description: description_of(inp.description()),
                    fields: input_fields(inp.input_fields_definition()),
                });
            }
            Definition::EnumTypeDefinition(en) => {
                let name = node_name(en.name());
                let variants: Vec<EnumVariant> = en
                    .enum_values_definition()
                    .into_iter()
                    .flat_map(|evd| {
                        evd.enum_value_definitions().map(|v| {
                            let original = node_name(v.enum_value().and_then(|ev| ev.name()));
                            let pascal = pascal_ident(&original);
                            let serde_rename = (pascal != original).then_some(original);
                            EnumVariant {
                                name: pascal,
                                serde_rename,
                            }
                        })
                    })
                    .collect();
                types.push(TypeDef::Enum {
                    name: pascal_ident(&name),
                    description: description_of(en.description()),
                    variants,
                });
            }
            Definition::ScalarTypeDefinition(s) => {
                let name = node_name(s.name());
                if name == "ID"
                    || name == "String"
                    || name == "Int"
                    || name == "Float"
                    || name == "Boolean"
                {
                    continue;
                }
                types.push(TypeDef::Alias {
                    name: pascal_ident(&name),
                    target: scalar_to_rust(&name),
                });
            }
            _ => {} // interfaces, unions, directives, fragments: skipped for now
        }
    }

    Ok(ApiSpec {
        name: crate_name("graphql_api"),
        display_name: "GraphQL API".to_string(),
        version: "0.1.0".to_string(),
        description: None,
        kind: ApiKind::GraphQl,
        base_url: None,
        types,
        operations,
    })
}

fn description_of(desc: Option<cst::Description>) -> Option<String> {
    let d = desc?;
    let sv = d.string_value()?;
    Some(sv.source_string().trim_matches('"').to_string())
}

fn node_name(name: Option<cst::Name>) -> String {
    name.map(|n| n.text().to_string()).unwrap_or_default()
}

fn object_fields(fd: Option<cst::FieldsDefinition>) -> Vec<Field> {
    let Some(fd) = fd else { return Vec::new() };
    fd.field_definitions()
        .map(|field| {
            let raw_name = node_name(field.name());
            let snake = snake_ident(&raw_name);
            let serde_rename =
                (snake.trim_start_matches("r#") != raw_name).then(|| raw_name.clone());
            let (inner, required) = field
                .ty()
                .map(|t| render_type(&t))
                .unwrap_or_else(|| ("serde_json::Value".to_string(), false));
            let rust_type = if required {
                inner
            } else {
                format!("Option<{inner}>")
            };
            Field {
                name: snake,
                serde_rename,
                rust_type,
                optional: !required,
                description: description_of(field.description()),
            }
        })
        .collect()
}

fn input_fields(idef: Option<cst::InputFieldsDefinition>) -> Vec<Field> {
    let Some(idef) = idef else { return Vec::new() };
    idef.input_value_definitions()
        .map(input_value_to_field)
        .collect()
}

fn input_value_to_field(ivd: InputValueDefinition) -> Field {
    let raw_name = node_name(ivd.name());
    let snake = snake_ident(&raw_name);
    let serde_rename = (snake.trim_start_matches("r#") != raw_name).then(|| raw_name.clone());
    let (inner, required) = ivd
        .ty()
        .map(|t| render_type(&t))
        .unwrap_or_else(|| ("serde_json::Value".to_string(), false));
    let rust_type = if required {
        inner
    } else {
        format!("Option<{inner}>")
    };
    Field {
        name: snake,
        serde_rename,
        rust_type,
        optional: !required,
        description: description_of(ivd.description()),
    }
}

fn field_to_operation(
    field: &FieldDefinition,
    http_method: HttpMethod,
    streaming: StreamingMode,
) -> Operation {
    let original_id = node_name(field.name());
    let id = snake_ident(&original_id);
    let (return_type, _required) = field
        .ty()
        .map(|t| render_type(&t))
        .unwrap_or_else(|| ("serde_json::Value".to_string(), false));

    let mut params = Vec::new();
    if let Some(args) = field.arguments_definition() {
        for ivd in args.input_value_definitions() {
            let raw_name = node_name(ivd.name());
            let (rust_type, required) = ivd
                .ty()
                .map(|t| render_type(&t))
                .unwrap_or_else(|| ("serde_json::Value".to_string(), false));
            params.push(Param {
                name: snake_ident(&raw_name),
                original_name: raw_name,
                rust_type,
                location: ParamLocation::GraphQlVariable,
                required,
                description: None,
            });
        }
    }

    Operation {
        id,
        original_id,
        description: description_of(field.description()),
        protocol: Protocol::GraphQl,
        endpoint: format!("{} /graphql", http_method.as_str()),
        http_method,
        params,
        return_type,
        streaming,
    }
}

/// Render a GraphQL type to a Rust type string + whether the outermost type
/// is non-null (required).
fn render_type(ty: &GqlType) -> (String, bool) {
    match ty {
        GqlType::NamedType(n) => {
            let name = node_name(n.name());
            (scalar_to_rust(&name), false)
        }
        GqlType::ListType(l) => {
            let inner = l.ty().map(|t| render_type(&t));
            let inner_str = match inner {
                Some((s, true)) => s,
                Some((s, false)) => format!("Option<{s}>"),
                None => "serde_json::Value".to_string(),
            };
            (format!("Vec<{inner_str}>"), false)
        }
        GqlType::NonNullType(n) => {
            // NonNullType always wraps either a NamedType or a ListType.
            if let Some(named) = n.named_type() {
                let name = node_name(named.name());
                (scalar_to_rust(&name), true)
            } else if let Some(list) = n.list_type() {
                // Recurse into the list, then mark required.
                let inner = list.ty().map(|t| render_type(&t));
                let inner_str = match inner {
                    Some((s, true)) => s,
                    Some((s, false)) => format!("Option<{s}>"),
                    None => "serde_json::Value".to_string(),
                };
                (format!("Vec<{inner_str}>"), true)
            } else {
                ("serde_json::Value".to_string(), true)
            }
        }
    }
}

fn scalar_to_rust(name: &str) -> String {
    match name {
        "Int" => "i32".into(),
        "Float" => "f64".into(),
        "String" => "String".into(),
        "Boolean" => "bool".into(),
        "ID" => "String".into(),
        other => pascal_ident(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const SCHEMA: &str = include_str!("../../tests/fixtures/schema.graphql");

    #[test]
    fn parses_object_types_and_enums() {
        let spec = parse(SCHEMA).unwrap();
        let user = spec.types.iter().find(|t| t.name() == "User").unwrap();
        match user {
            TypeDef::Struct { fields, .. } => {
                let id_field = fields.iter().find(|f| f.name == "id").unwrap();
                assert!(!id_field.optional);
                let email_field = fields.iter().find(|f| f.name == "email").unwrap();
                assert!(email_field.optional);
                assert!(email_field.rust_type.starts_with("Option<"));
            }
            _ => panic!("expected struct"),
        }

        let role = spec.types.iter().find(|t| t.name() == "Role").unwrap();
        match role {
            TypeDef::Enum { variants, .. } => {
                assert!(variants.iter().any(|v| v.name == "Admin"));
                assert!(variants.iter().any(|v| v.name == "Member"));
            }
            _ => panic!("expected enum"),
        }
    }

    #[test]
    fn extracts_query_and_mutation_ops() {
        let spec = parse(SCHEMA).unwrap();
        let ops: Vec<_> = spec
            .operations
            .iter()
            .map(|o| o.original_id.as_str())
            .collect();
        assert!(ops.contains(&"user"));
        assert!(ops.contains(&"posts"));
        assert!(ops.contains(&"createPost"));

        let user_op = spec
            .operations
            .iter()
            .find(|o| o.original_id == "user")
            .unwrap();
        assert_eq!(user_op.protocol, Protocol::GraphQl);
        let id_arg = user_op
            .params
            .iter()
            .find(|p| p.original_name == "id")
            .unwrap();
        assert!(id_arg.required);
        assert_eq!(id_arg.location, ParamLocation::GraphQlVariable);
        assert_eq!(user_op.streaming, StreamingMode::Unary);
    }

    #[test]
    fn subscription_fields_become_server_streams() {
        let spec = parse(SCHEMA).unwrap();
        let sub = spec
            .operations
            .iter()
            .find(|o| o.original_id == "postCreated")
            .expect("subscription field");
        assert_eq!(sub.streaming, StreamingMode::ServerStream);
        assert_eq!(sub.protocol, Protocol::GraphQl);
    }
}
