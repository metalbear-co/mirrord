Incoming config now supports off mode, which is also used when `"incoming": "off"`.
When incoming is off, listen requests go through.
Changed targetless to warn on listen, since bind can happen on outgoing sockets as well.
