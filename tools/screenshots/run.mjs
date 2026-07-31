import mascot, { workspace } from "./scenes.mjs";
export default async function (api) {
  await mascot(api);
  await workspace(api);
}
