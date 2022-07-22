use quote::{quote, ToTokens};
use syn::{ItemFn, Signature, TypeBareFn};

#[proc_macro_attribute]
pub fn proc_hook(
    args: proc_macro::TokenStream,
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let output: proc_macro2::TokenStream = {
        let proper = input.clone();
        let proper_function = syn::parse_macro_input!(proper as ItemFn);

        let signature = proper_function.clone().sig;

        let unsafety = signature.unsafety.unwrap();
        let abi = signature.abi.unwrap();

        let fn_args = signature
            .inputs
            .into_iter()
            .map(|fn_arg| match fn_arg {
                syn::FnArg::Receiver(_) => panic!("Invalid argument type!"),
                syn::FnArg::Typed(arg) => arg.ty,
            })
            .collect::<Vec<_>>();

        let variadic = signature.variadic;

        let return_type = signature.output;

        let bare_fn = quote! {
            #unsafety #abi fn(#(#fn_args),*, #variadic) #return_type;
        };

        // panic!("output is \n{:#?}", bare_fn);

        let output = quote! {
            type Foo = #bare_fn

            #proper_function

        };

        // panic!("{:#?}", output.to_string());

        output.into()
    };

    proc_macro::TokenStream::from(output)
}
