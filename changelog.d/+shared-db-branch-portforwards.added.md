Keep database branch port forwards alive while another local mirrord session is
using them. Sessions attach to a shared local forward, and the forward closes
automatically after the last session exits or crashes.
