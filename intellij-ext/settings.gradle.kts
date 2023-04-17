rootProject.name = "mirrord"


include (
    "modules/core",
    "modules/products/idea",
    "modules/products/goland",
    "modules/products/pycharm",
    "modules/products/rubymine",
    "modules/products/webstorm",
)

// Rename modules to mirrord-<module>, I think this is required IntelliJ wise.
rootProject.children.forEach {
    it.name = (it.name.replaceFirst("modules/", "mirrord/").replace("/", "-"))
}