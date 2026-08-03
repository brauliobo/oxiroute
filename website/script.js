(function () {
  'use strict';

  function activateTab(tabset, button) {
    var tabId = button.getAttribute('data-tab');
    if (!tabId) return;

    tabset.querySelectorAll('[role="tab"]').forEach(function (tab) {
      var selected = tab === button;
      tab.classList.toggle('is-active', selected);
      tab.setAttribute('aria-selected', String(selected));
      tab.setAttribute('tabindex', selected ? '0' : '-1');
    });

    tabset.querySelectorAll('[data-panel]').forEach(function (panel) {
      var selected = panel.id === tabId;
      panel.classList.toggle('is-active', selected);
      panel.hidden = !selected;
    });
  }

  document.querySelectorAll('[data-tabset]').forEach(function (tabset) {
    var buttons = Array.from(tabset.querySelectorAll('[role="tab"]'));
    var initial = buttons.find(function (button) { return button.classList.contains('is-active'); }) || buttons[0];
    if (initial) activateTab(tabset, initial);

    buttons.forEach(function (button, index) {
      button.addEventListener('click', function () { activateTab(tabset, button); });
      button.addEventListener('keydown', function (event) {
        var nextIndex = index;
        if (event.key === 'ArrowRight' || event.key === 'ArrowDown') nextIndex = (index + 1) % buttons.length;
        if (event.key === 'ArrowLeft' || event.key === 'ArrowUp') nextIndex = (index - 1 + buttons.length) % buttons.length;
        if (event.key === 'Home') nextIndex = 0;
        if (event.key === 'End') nextIndex = buttons.length - 1;
        if (nextIndex === index) return;
        event.preventDefault();
        buttons[nextIndex].focus();
        activateTab(tabset, buttons[nextIndex]);
      });
    });
  });

  document.querySelectorAll('[data-copy]').forEach(function (button) {
    button.addEventListener('click', function () {
      var target = document.getElementById(button.getAttribute('data-copy'));
      if (!target || !navigator.clipboard) return;
      navigator.clipboard.writeText(target.innerText).then(function () {
        var original = button.textContent;
        button.textContent = 'Copied';
        window.setTimeout(function () { button.textContent = original; }, 1400);
      });
    });
  });
}());
