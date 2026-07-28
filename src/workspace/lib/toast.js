import { toast, toastText } from "./dom.js";

export function showToast(msg) {
  toastText.textContent = msg;
  toast.classList.add("show");
  setTimeout(() => toast.classList.remove("show"), 1800);
}
