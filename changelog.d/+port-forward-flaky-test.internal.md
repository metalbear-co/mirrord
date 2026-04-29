Fix `multiple_mappings_port_forwarding` flakiness by letting `PortForwarder` own the listeners directly instead of binding/dropping/rebinding ephemeral ports.
