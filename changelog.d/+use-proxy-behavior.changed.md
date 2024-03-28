`use_proxy` behavior is now setting the proxy env to empty value instead of unsetting. This should help with cases where
we need it to propogate to the extensions.