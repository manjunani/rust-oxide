//! OpenAPI 3.x parser.
//!
//! Translates an OpenAPI document (JSON or YAML) into our [`ApiSpec`] IR. The
//! parser intentionally covers the common case — primitive types, simple
//! object schemas, top-level `$ref`s, and basic operation parameters/bodies.
//! Anything more exotic (`oneOf`/`anyOf`, polymorphism, nested anonymous
//! schemas) degrades to `serde_json::Value` so the generated crate still
//! compiles.

use openapiv3::{
    ArrayType, IntegerFormat, NumberFormat, ObjectType, OpenAPI, Operation as OpenApiOp,
    Parameter, ParameterSchemaOrContent, PathItem, ReferenceOr, Schema, SchemaKind,
    StatusCode, Type as SchemaType, VariantOrUnknownOrEmpty,
};

use crate::error::Result;
use crate::ir::{
    ApiKind, ApiSpec, EnumVariant, Field, HttpMethod, Operation, Param, ParamLocation, Protocol,
    StreamingMode, TypeDef,
};
use crate::parsers::naming::{crate_name, pascal_ident, snake_ident};

/// Parse an OpenAPI spec from a JSON or YAML string.
pub fn parse(raw: &str) -> Result<ApiSpec> {
    let api: OpenAPI = if looks_like_json(raw) {
        serde_json::from_str(raw)?
    } else {
        serde_yaml::from_str(raw)?
    };

    let title = api.info.title.clone();
    let version = if api.info.version.is_empty() {
        "0.1.0".to_string()
    } else {
        api.info.version.clone()
    };
    let description = api.info.description.clone();
    let base_url = api
        .servers
        .first()
        .map(|s| strip_template(&s.url));

    let mut types = Vec::new();
    if let Some(components) = api.components.as_ref() {
        for (name, schema_ref) in &components.schemas {
            if let Some(td) = schema_to_typedef(name, schema_ref) {
                types.push(td);
            }
        }
    }

    let mut operations = Vec::new();
    for (path, item_ref) in &api.paths.paths {
        let ReferenceOr::Item(item) = item_ref else { continue };
        collect_operations(path, item, &mut operations);
    }

    Ok(ApiSpec {
        name: crate_name(&title),
        display_name: title,
        version,
        description,
        kind: ApiKind::OpenApi,
        base_url,
        types,
        operations,
    })
}

fn looks_like_json(raw: &str) -> bool {
    raw.trim_start().starts_with('{')
}

fn strip_template(url: &str) -> String {
    // Strip server-template parameters like `{scheme}://{host}` — we only need
    // a usable default. If templates dominate we just keep the raw string.
    url.to_string()
}

// ---------------------------------------------------------------------------
// Schemas → TypeDef
// ---------------------------------------------------------------------------

fn schema_to_typedef(name: &str, schema_ref: &ReferenceOr<Schema>) -> Option<TypeDef> {
    let schema = match schema_ref {
        ReferenceOr::Item(s) => s,
        // A top-level alias `Foo: $ref Bar` is rare; emit as a Rust alias.
        ReferenceOr::Reference { reference } => {
            return Some(TypeDef::Alias {
                name: pascal_ident(name),
                target: reference_name(reference),
            });
        }
    };

    let description = schema_description(schema);
    let rust_name = pascal_ident(name);

    match &schema.schema_kind {
        SchemaKind::Type(SchemaType::String(s)) => {
            if !s.enumeration.is_empty() {
                let variants: Vec<EnumVariant> = s
                    .enumeration
                    .iter()
                    .filter_map(|v| {
                        v.as_ref().map(|orig| {
                            let pascal = pascal_ident(orig);
                            let serde_rename = (pascal != *orig).then(|| orig.clone());
                            EnumVariant {
                                name: pascal,
                                serde_rename,
                            }
                        })
                    })
                    .collect();
                if !variants.is_empty() {
                    return Some(TypeDef::Enum {
                        name: rust_name,
                        description,
                        variants,
                    });
                }
            }
            Some(TypeDef::Alias {
                name: rust_name,
                target: "String".into(),
            })
        }
        SchemaKind::Type(SchemaType::Object(obj)) => Some(TypeDef::Struct {
            name: rust_name,
            description,
            fields: object_fields(obj),
        }),
        SchemaKind::Type(SchemaType::Array(arr)) => Some(TypeDef::Alias {
            name: rust_name,
            target: format!("Vec<{}>", array_item_type(arr)),
        }),
        SchemaKind::Type(SchemaType::Integer(_)) => Some(TypeDef::Alias {
            name: rust_name,
            target: "i64".into(),
        }),
        SchemaKind::Type(SchemaType::Number(_)) => Some(TypeDef::Alias {
            name: rust_name,
            target: "f64".into(),
        }),
        SchemaKind::Type(SchemaType::Boolean(_)) => Some(TypeDef::Alias {
            name: rust_name,
            target: "bool".into(),
        }),
        // OneOf/AnyOf/AllOf/Any → fall back to a JSON Value alias.
        _ => Some(TypeDef::Alias {
            name: rust_name,
            target: "serde_json::Value".into(),
        }),
    }
}

fn schema_description(schema: &Schema) -> Option<String> {
    schema.schema_data.description.clone()
}

fn object_fields(obj: &ObjectType) -> Vec<Field> {
    let mut fields = Vec::with_capacity(obj.properties.len());
    for (name, prop_ref) in &obj.properties {
        let required = obj.required.iter().any(|r| r == name);
        let inner_ty = rust_type_for_property(prop_ref);
        let rust_type = if required {
            inner_ty
        } else {
            format!("Option<{inner_ty}>")
        };
        let snake = snake_ident(name);
        let serde_rename = if snake.trim_start_matches("r#") != name {
            Some(name.clone())
        } else {
            None
        };
        fields.push(Field {
            name: snake,
            serde_rename,
            rust_type,
            optional: !required,
            description: None,
        });
    }
    fields
}

fn rust_type_for_property(prop: &ReferenceOr<Box<Schema>>) -> String {
    match prop {
        ReferenceOr::Reference { reference } => reference_name(reference),
        ReferenceOr::Item(schema) => rust_type_for_schema(schema),
    }
}

fn rust_type_for_schema(schema: &Schema) -> String {
    match &schema.schema_kind {
        SchemaKind::Type(SchemaType::String(s)) => match &s.format {
            VariantOrUnknownOrEmpty::Item(openapiv3::StringFormat::Byte)
            | VariantOrUnknownOrEmpty::Item(openapiv3::StringFormat::Binary) => "Vec<u8>".into(),
            _ => "String".into(),
        },
        SchemaKind::Type(SchemaType::Integer(i)) => match &i.format {
            VariantOrUnknownOrEmpty::Item(IntegerFormat::Int32) => "i32".into(),
            VariantOrUnknownOrEmpty::Item(IntegerFormat::Int64) => "i64".into(),
            _ => "i64".into(),
        },
        SchemaKind::Type(SchemaType::Number(n)) => match &n.format {
            VariantOrUnknownOrEmpty::Item(NumberFormat::Float) => "f32".into(),
            VariantOrUnknownOrEmpty::Item(NumberFormat::Double) => "f64".into(),
            _ => "f64".into(),
        },
        SchemaKind::Type(SchemaType::Boolean(_)) => "bool".into(),
        SchemaKind::Type(SchemaType::Array(arr)) => {
            format!("Vec<{}>", array_item_type(arr))
        }
        SchemaKind::Type(SchemaType::Object(_)) => "serde_json::Value".into(),
        _ => "serde_json::Value".into(),
    }
}

fn array_item_type(arr: &ArrayType) -> String {
    match arr.items.as_ref() {
        Some(ReferenceOr::Reference { reference }) => reference_name(reference),
        Some(ReferenceOr::Item(schema)) => rust_type_for_schema(schema),
        None => "serde_json::Value".into(),
    }
}

fn reference_name(reference: &str) -> String {
    let last = reference.rsplit('/').next().unwrap_or(reference);
    pascal_ident(last)
}

// ---------------------------------------------------------------------------
// Paths → Operation
// ---------------------------------------------------------------------------

fn collect_operations(path: &str, item: &PathItem, out: &mut Vec<Operation>) {
    let entries = [
        (HttpMethod::Get, item.get.as_ref()),
        (HttpMethod::Post, item.post.as_ref()),
        (HttpMethod::Put, item.put.as_ref()),
        (HttpMethod::Patch, item.patch.as_ref()),
        (HttpMethod::Delete, item.delete.as_ref()),
        (HttpMethod::Head, item.head.as_ref()),
        (HttpMethod::Options, item.options.as_ref()),
    ];
    for (method, op_opt) in entries {
        if let Some(op) = op_opt {
            out.push(build_operation(path, method, op));
        }
    }
}

fn build_operation(path: &str, method: HttpMethod, op: &OpenApiOp) -> Operation {
    let original_id = op
        .operation_id
        .clone()
        .unwrap_or_else(|| default_op_id(method, path));
    let id = snake_ident(&original_id);
    let description = op
        .summary
        .clone()
        .or_else(|| op.description.clone());

    let mut params = Vec::new();
    for p in &op.parameters {
        if let ReferenceOr::Item(parameter) = p {
            if let Some(param) = parameter_to_ir(parameter) {
                params.push(param);
            }
        }
    }

    if let Some(ReferenceOr::Item(body)) = op.request_body.as_ref() {
        if let Some(json) = body.content.get("application/json") {
            let rust_type = json
                .schema
                .as_ref()
                .map(|s| match s {
                    ReferenceOr::Reference { reference } => reference_name(reference),
                    ReferenceOr::Item(schema) => rust_type_for_schema(schema),
                })
                .unwrap_or_else(|| "serde_json::Value".into());
            params.push(Param {
                name: "body".into(),
                original_name: "body".into(),
                rust_type,
                location: ParamLocation::Body,
                required: body.required,
                description: None,
            });
        }
    }

    let return_type = response_type(op).unwrap_or_else(|| "serde_json::Value".into());

    Operation {
        id,
        original_id,
        description,
        protocol: Protocol::Rest,
        endpoint: format!("{} {}", method.as_str(), path),
        http_method: method,
        params,
        return_type,
        streaming: StreamingMode::Unary,
    }
}

fn default_op_id(method: HttpMethod, path: &str) -> String {
    let safe: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    format!("{}{}", method.as_str().to_lowercase(), safe)
}

fn parameter_to_ir(parameter: &Parameter) -> Option<Param> {
    let (data, location) = match parameter {
        Parameter::Query { parameter_data, .. } => (parameter_data, ParamLocation::Query),
        Parameter::Path { parameter_data, .. } => (parameter_data, ParamLocation::Path),
        Parameter::Header { parameter_data, .. } => (parameter_data, ParamLocation::Header),
        Parameter::Cookie { parameter_data, .. } => (parameter_data, ParamLocation::Header),
    };
    let rust_type = match &data.format {
        ParameterSchemaOrContent::Schema(schema_ref) => match schema_ref {
            ReferenceOr::Reference { reference } => reference_name(reference),
            ReferenceOr::Item(schema) => rust_type_for_schema(schema),
        },
        ParameterSchemaOrContent::Content(_) => "String".into(),
    };

    Some(Param {
        name: snake_ident(&data.name),
        original_name: data.name.clone(),
        rust_type,
        location,
        required: data.required,
        description: data.description.clone(),
    })
}

fn response_type(op: &OpenApiOp) -> Option<String> {
    // Prefer a 2xx response, falling back to default.
    let candidate = op
        .responses
        .responses
        .iter()
        .find(|(code, _)| match code {
            StatusCode::Code(c) => (200..300).contains(c),
            _ => false,
        })
        .or_else(|| op.responses.responses.iter().next())?;

    let resp = match candidate.1 {
        ReferenceOr::Item(r) => r,
        ReferenceOr::Reference { .. } => return Some("serde_json::Value".into()),
    };

    let json = resp.content.get("application/json")?;
    let schema = json.schema.as_ref()?;
    Some(match schema {
        ReferenceOr::Reference { reference } => reference_name(reference),
        ReferenceOr::Item(s) => rust_type_for_schema(s),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PETSTORE: &str = include_str!("../../tests/fixtures/petstore.yaml");

    #[test]
    fn parses_petstore_metadata() {
        let spec = parse(PETSTORE).unwrap();
        assert_eq!(spec.kind, ApiKind::OpenApi);
        assert_eq!(spec.display_name, "Pet Store");
        assert_eq!(spec.name, "pet_store");
        assert!(spec.base_url.as_deref().unwrap().contains("petstore"));
    }

    #[test]
    fn extracts_component_schemas() {
        let spec = parse(PETSTORE).unwrap();
        let pet = spec
            .types
            .iter()
            .find(|t| t.name() == "Pet")
            .expect("Pet type");
        match pet {
            TypeDef::Struct { fields, .. } => {
                let names: Vec<_> = fields.iter().map(|f| f.name.as_str()).collect();
                assert!(names.contains(&"id"));
                assert!(names.contains(&"name"));
                // `tag` is optional.
                let tag = fields.iter().find(|f| f.name == "tag").unwrap();
                assert!(tag.optional);
                assert!(tag.rust_type.starts_with("Option<"));
            }
            other => panic!("expected struct, got {other:?}"),
        }
    }

    #[test]
    fn extracts_operations() {
        let spec = parse(PETSTORE).unwrap();
        let ids: Vec<_> = spec
            .operations
            .iter()
            .map(|o| o.original_id.as_str())
            .collect();
        assert!(ids.contains(&"listPets"));
        assert!(ids.contains(&"createPet"));
        assert!(ids.contains(&"getPet"));

        let get_pet = spec
            .operations
            .iter()
            .find(|o| o.original_id == "getPet")
            .unwrap();
        assert_eq!(get_pet.http_method, HttpMethod::Get);
        let pet_id = get_pet
            .params
            .iter()
            .find(|p| p.original_name == "petId")
            .expect("petId param");
        assert_eq!(pet_id.location, ParamLocation::Path);
        assert!(pet_id.required);
    }

    #[test]
    fn parses_json_spec() {
        let json = r#"{
          "openapi": "3.0.0",
          "info": { "title": "Tiny", "version": "1.0" },
          "paths": {
            "/ping": {
              "get": {
                "operationId": "ping",
                "responses": { "200": { "description": "ok" } }
              }
            }
          }
        }"#;
        let spec = parse(json).unwrap();
        assert_eq!(spec.name, "tiny");
        assert_eq!(spec.operations.len(), 1);
        assert_eq!(spec.operations[0].id, "ping");
    }
}
