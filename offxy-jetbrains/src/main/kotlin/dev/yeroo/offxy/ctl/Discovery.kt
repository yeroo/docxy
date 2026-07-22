package dev.yeroo.offxy.ctl

import java.nio.file.Files
import java.nio.file.Path
import java.nio.file.Paths
import java.nio.file.StandardCopyOption

/** Discovery-file plumbing, ctlcore-compatible: `dir/<instance>.json` holding
 *  `{"instance","port","token","pid"}`, written atomically (tmp + rename). */
object Discovery {
    /** An app's ctl dir: `%APPDATA%\<app>\ctl` (Windows) or
     *  `$XDG_CONFIG_HOME/<app>/ctl` (fallback `~/.config`). The
     *  `offxy.ctl.dir` system property overrides the base for tests
     *  (files land under `<override>/<app>`). */
    fun ctlDir(app: String = "docxy"): Path {
        System.getProperty("offxy.ctl.dir")?.let { return Paths.get(it).resolve(app) }
        val appData = System.getenv("APPDATA")
        val base = when {
            System.getProperty("os.name", "").startsWith("Windows") && appData != null ->
                Paths.get(appData)
            else ->
                System.getenv("XDG_CONFIG_HOME")?.let { Paths.get(it) }
                    ?: Paths.get(System.getProperty("user.home"), ".config")
        }
        return base.resolve(app).resolve("ctl")
    }

    /** Instance-name sanitizer, matching the agent-access convention:
     *  lowercase, every non-alphanumeric run collapsed to `-`. */
    fun sanitize(name: String): String =
        name.lowercase().replace(Regex("[^a-z0-9]+"), "-").trim('-')

    fun path(dir: Path, instance: String): Path = dir.resolve("$instance.json")

    fun write(dir: Path, instance: String, port: Int, token: String) {
        Files.createDirectories(dir)
        val contents = dev.yeroo.offxy.editor.Json.write(
            linkedMapOf(
                "instance" to instance,
                "port" to port,
                "token" to token,
                "pid" to ProcessHandle.current().pid(),
            ),
        )
        val tmp = dir.resolve("$instance.json.${ProcessHandle.current().pid()}.tmp")
        Files.write(tmp, contents.toByteArray(Charsets.UTF_8))
        Files.move(tmp, path(dir, instance), StandardCopyOption.REPLACE_EXISTING)
    }

    fun delete(dir: Path, instance: String) {
        runCatching { Files.deleteIfExists(path(dir, instance)) }
    }
}
