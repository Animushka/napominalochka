const appearance = window.matchMedia("(prefers-color-scheme: dark)");
const updateAppearance = () => document.documentElement.classList.toggle("dark", appearance.matches);
updateAppearance();
appearance.addEventListener("change", updateAppearance);
