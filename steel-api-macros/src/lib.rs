//! Procedural macros for the Steel API plugin system.

use proc_macro::TokenStream;
use quote::quote;
use syn::{ItemFn, parse_macro_input};

/// Marks a function as the plugin entry point.
///
/// This macro generates the necessary FFI exports for the plugin loader to discover
/// and load the plugin. The function body should return a `Plugin` instance.
#[proc_macro_attribute]
pub fn export_plugin(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let input_fn = parse_macro_input!(item as ItemFn);
    let stmts = &input_fn.block.stmts;

    let expanded = quote! {
        #[unsafe(no_mangle)]
        extern "C" fn __plugin_root() -> ::steel_api::Plugin {
            #(#stmts)*
        }

        #[unsafe(no_mangle)]
        extern "C" fn __plugin_root__report() -> ::steel_api::PluginReport {
            ::steel_api::PluginReport {
                abi_header: ::steel_api::LIB_HEADER,
                type_layout: <::steel_api::Plugin as ::steel_api::StableAbi>::LAYOUT,
            }
        }
    };

    expanded.into()
}
