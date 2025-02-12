use std::sync::Arc;

use swc::{
    config::{JscConfig, SourceMapsConfig},
    Compiler,
};

use swc_common::{self, FileName, SourceMap};
use swc_ecma_ast::EsVersion;
use swc_ecma_parser::{Syntax, TsConfig};

pub fn compile_typescript(input: &str) -> Result<CompiledItem, String> {
    compile_typescript_inner(input)
}

fn compile_typescript_inner(input: &str) -> Result<CompiledItem, String> {
    swc_common::GLOBALS.set(&Default::default(), || {
        let cm: Arc<SourceMap> = Arc::new(SourceMap::default());

        let c = Compiler::new(cm.clone());
        let fm = cm.new_source_file(FileName::Custom("script.ts".into()), input.into());

        match swc::try_with_handler(
            cm,
            swc::HandlerOpts {
                color: swc_common::errors::ColorConfig::Never,
                skip_filename: false,
            },
            |handler| {
                c.process_js_file(
                    fm,
                    handler,
                    &swc::config::Options {
                        config: swc::config::Config {
                            jsc: JscConfig {
                                syntax: Some(Syntax::Typescript(TsConfig {
                                    ..Default::default()
                                })),
                                target: Some(EsVersion::Es2022),
                                ..Default::default()
                            },
                            ..Default::default()
                        },
                        source_maps: Some(SourceMapsConfig::Bool(true)),
                        ..Default::default()
                    },
                )
            },
        ) {
            Ok(output) => {
                let map_raw = output.map.unwrap();
                let map_parsed = sourcemap::SourceMap::from_slice(map_raw.as_bytes()).unwrap();

                Ok(CompiledItem {
                    output: output.code,
                    source_map: map_parsed,
                    source_map_raw: map_raw,
                })
            }
            Err(err) => Err(err.to_string()),
        }
    })
}

#[derive(Debug, Clone)]
pub struct CompiledItem {
    pub output: String,
    pub source_map: sourcemap::SourceMap,
    pub source_map_raw: String,
}

#[cfg(test)]
mod tests {
    use crate::compile_typescript;

    fn compile(input: &str, expected_output: &str) {
        let output = compile_typescript(input).unwrap();
        assert_eq!(output.output, expected_output);
    }

    #[test]
    fn tst_simple() {
        compile("let a: string = 'asd'", "let a = 'asd';\n");
    }
}
