rootProject.name = "mirrord"


include (
    "modules/core",
    "modules/products/idea",
    "modules/products/goland",
    "modules/products/pycharm",
    "modules/products/rubymine",
)

rootProject.children.forEach {
    it.name = (it.name.replaceFirst("modules/", "mirrord/").replace("/", "-"))
}