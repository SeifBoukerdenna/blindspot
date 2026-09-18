# Welcome to Blindspot

Your Mac, a few keystrokes away. No account or AI model is needed to begin.

## Install and open

1. Download **Blindspot-x.y.z.zip** from the official GitHub release. The “Source code”
   archives are for developers; they are not the app.
2. Unzip the download and move **Blindspot.app** into Applications. Applications in your
   home folder (`~/Applications`) also works and supports updates without administrator access.
3. Open Blindspot. It is not notarized by Apple. If macOS blocks this first launch, open
   **System Settings → Privacy & Security → Open Anyway** for the copy you downloaded.
   See [Apple's instructions](https://support.apple.com/en-gb/102445).
4. The welcome window opens. Choose **Make it yours**, or **Open Blindspot** to start immediately.

Requires macOS 26 or later on Apple silicon. If the download is damaged or macOS reports malware,
use the official release/support information rather than disabling system security.

## Your first useful shortcut

Press **⌘⇧Space**, type **Safari**, then press **Return**. Press the shortcut again to come back.
The setup guide lets you test/change your shortcut and choose whether Blindspot opens at login.
There is no Dock icon. The menu-bar command symbol offers **Open Blindspot**, **Set up Blindspot…**,
and **Settings…**. Reopening the app in Finder also opens Settings.

**⌘,** opens Settings. **Escape** closes the launcher. **:** shows recent commands first;
**:help** lists the complete catalog.

## Add the features you want

Open **Set up Blindspot…** in the menu bar or **Settings → General**. Every section is optional,
and the guide remembers where you left off. Existing installations retain their preferences.
Choose **Restart onboarding…** from the menu-bar command symbol to begin again at Welcome.
This keeps your settings, indexed documents, installed models and any ongoing setup download.
On a new installation, content indexing and clipboard capture wait for your choices.

- **Documents:** choose a small notes/document folder, then **Start indexing**. Allow access if
  macOS asks. Search can become useful while the remaining files are processed. Open the launcher,
  choose Documents and search a phrase from a file. **⌘Y** reads its matching passage.
- **Indexing help:** the guide shows status and pause/resume. Settings → Index has detailed folder
  and file diagnostics. Download cloud-only files in Finder before retrying; Blindspot will not
  download your cloud library. An empty or inaccessible folder does not require erasing the index.
- **Clipboard:** turn history on, copy an innocuous line, then type **;** in Blindspot to find it.
  Image capture and image text recognition are separate choices. Disable capture or clear existing
  history in Settings → Clipboard.
- **Accessibility:** the guide opens **Privacy & Security → Accessibility**. Enable Blindspot;
  if missing, add the installed app with **+**. Return to the guide and test selection/insertion
  in its sample field. The test does not change your clipboard. Ordinary launching/search needs
  no Accessibility permission.

## Your first local AI model

1. In **Set up Blindspot → Local AI**, choose **Download Ollama**. Open its DMG, drag Ollama
   into Applications, and open it. Blindspot does not require the optional command-line tool.
2. Return and **Check connection**. If Ollama is already running, reuse it.
3. Choose the suggested model for your Mac's memory, a smaller download, or an installed model.
   These are starting suggestions, not guaranteed speed or memory-fit claims.
4. Choose **Download model…** and confirm the displayed size. Ollama uses the internet to obtain
   the model; documents are not part of this download. Keep extra disk space for working files.
5. Watch progress. Setup verifies the local model and tests a sample response before saving it
   for writing and questions. **Test & use this model** does the same for an installed model.
6. Open Blindspot, type **>** and ask a question. Once documents are indexed, try
   **>docs What does my trail checklist say about rain gear?** and open a source citation.
   If you previously picked a model with **⌘M**, choose **Automatic** there to use the
   model saved by setup; a manual model selection takes precedence over Settings.

No model is downloaded without confirmation. A failed or interrupted request can be retried;
existing models are kept. Stopping disconnects Blindspot's request; another Ollama client may
still be using the same download. Closing setup offers to keep work running or stop the request.
If a model will not load, choose a smaller one. See Settings → AI for advanced configuration.

**Search by meaning** uses a separate embedding model. After testing your answer model, expand
**Add search by meaning** for its optional download/test. Word search works without this model.
Preparing embeddings is separate from downloading a model or indexing document text.

## Local containers

If Docker or a Podman machine is already running, type **:containers** and press Return.
Select a local engine. **Containers** shows running and stopped instances; **Images** shows
installed templates, so the counts can differ.

- **Images → Create container…:** configure a name, optional localhost port pair, .env file and
  masked overrides, mounts, restart policy and CPU/memory limits. Review before creating.
- **Overview:** inspect metadata and use confirmed Start/Stop/Restart controls.
- **Resources / Events:** start monitoring for bounded resource charts and lifecycle events.
- **Logs:** read or follow timestamped output, search it, pause/reconnect, or copy/export the
  loaded or filtered view. Exports go to a local file you choose.

The window remains open when focus changes. Uncheck **Keep on top** for normal stacking;
**⌘W** closes it and stops monitoring. Blindspot does not install runtimes or pull images.
Environment configuration applies only to new containers. See the complete guide for limits.

## Keep going

**⌘K** shows result actions. **⌘Y** previews. Calendar, screen capture and app-control permissions
are requested when needed; you can decline and continue using other features.

Updates are manual: **Settings → About → Updates → Check for updates**. Keep the installed app
in a writable Applications folder. Your settings and history survive normal updates.

The complete reference ships beside this file as **FEATURE-GUIDE.md**, and is also available
[in the repository](https://github.com/SeifBoukerdenna/blindspot/blob/main/docs/new-features.md).
