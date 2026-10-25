import { preview } from "vite";
import { startService } from "./local-service.mjs";

const service = await startService();
try {
  const server = await preview({
    preview: {
      host: "127.0.0.1",
      port: 4173,
      strictPort: true,
      proxy: { "/api": service.origin, "/health": service.origin },
    },
  });
  let stopping = false;
  const close = async () => {
    if (stopping) return;
    stopping = true;
    await new Promise((resolve) => server.httpServer.close(resolve));
    await service.close();
  };
  process.once("SIGTERM", close);
  process.once("SIGINT", close);
  console.log("Isolated local admin preview: http://127.0.0.1:4173/admin/");
} catch (error) {
  await service.close();
  throw error;
}
