const { app, BrowserWindow } = require("electron");

app.whenReady().then(() => {
  const window = new BrowserWindow({
    width: 1280,
    height: 900,
    show: true,
    title: "Electron baseline",
    webPreferences: {
      sandbox: true,
      contextIsolation: true,
      nodeIntegration: false,
    },
  });

  window.loadURL(
    "data:text/html;charset=utf-8,<title>Electron baseline</title><main>Electron baseline</main>",
  );
});

app.on("window-all-closed", () => app.quit());
