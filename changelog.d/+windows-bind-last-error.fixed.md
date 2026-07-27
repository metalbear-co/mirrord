Fixed Windows socket hooks replacing a failed `bind` call's WinSock error while closing the tracing span, which made applications receive an unrelated error code.
