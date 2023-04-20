rootProject.name = "mirrord"


include (
    "modules/core",
    "modules/products/idea",
    "modules/products/goland",
    "modules/products/pycharm",
    // new is targeting newer pycharm versions (222>) while old targets both.
    "modules/products/pycharm-new",
    "modules/products/rubymine",
    "modules/products/nodejs",
)

// Rename modules to mirrord-<module>, I think this is required IntelliJ wise.
rootProject.children.forEach {
    it.name = (it.name.replaceFirst("modules/", "mirrord/").replace("/", "-"))
}