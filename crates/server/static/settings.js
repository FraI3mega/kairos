(() => {
  const select = document.getElementById('timezone');
  if (!select) return;

  let zone;
  try {
    zone = Intl.DateTimeFormat().resolvedOptions().timeZone;
  } catch {
    return;
  }
  if (!zone || select.dataset.current !== 'UTC' || zone === 'UTC') return;

  const index = [...select.options].findIndex((option) => option.value === zone);
  if (index === -1) return;
  select.selectedIndex = index;
  document.getElementById('tzdetected').textContent = zone;
  document.getElementById('tznote').hidden = false;
})();
