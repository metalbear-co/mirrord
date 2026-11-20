import { copyFile } from "fs/promises";

await copyFile("/app/dummy", "/app/dummy.copy")
