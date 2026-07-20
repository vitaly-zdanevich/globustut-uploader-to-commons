(() => {
  'use strict';

  const formatSize = (bytes) => {
    const units = ['Б', 'КБ', 'МБ', 'ГБ'];
    let value = bytes;
    let unit = 0;
    while (value >= 1024 && unit < units.length - 1) {
      value /= 1024;
      unit += 1;
    }
    return unit === 0 ? `${value} ${units[unit]}` : `${value.toFixed(1)} ${units[unit]}`;
  };

  const setupFiles = () => {
    const input = document.querySelector('[data-file-input]');
    const drop = document.querySelector('[data-file-drop]');
    const list = document.querySelector('[data-selected-files]');
    if (!(input instanceof HTMLInputElement) || !drop || !list) return;

    let files = [];
    const syncInput = () => {
      const transfer = new DataTransfer();
      files.forEach((file) => transfer.items.add(file));
      input.files = transfer.files;
    };
    const render = () => {
      list.replaceChildren();
      files.forEach((file, index) => {
        const row = document.createElement('div');
        row.className = 'selected-file';
        const name = document.createElement('span');
        name.className = 'selected-file-name';
        name.textContent = file.name;
        const size = document.createElement('span');
        size.className = 'selected-file-size';
        size.textContent = formatSize(file.size);
        const remove = document.createElement('button');
        remove.type = 'button';
        remove.className = 'icon-button';
        remove.textContent = '×';
        remove.title = `Удалить ${file.name}`;
        remove.setAttribute('aria-label', `Удалить ${file.name}`);
        remove.addEventListener('click', () => {
          files.splice(index, 1);
          syncInput();
          render();
        });
        row.append(name, size, remove);
        list.append(row);
      });
    };
    const replaceFiles = (incoming) => {
      files = Array.from(incoming);
      syncInput();
      render();
    };
    input.addEventListener('change', () => replaceFiles(input.files ?? []));
    ['dragenter', 'dragover'].forEach((name) => drop.addEventListener(name, (event) => {
      event.preventDefault();
      drop.classList.add('is-dragging');
    }));
    ['dragleave', 'drop'].forEach((name) => drop.addEventListener(name, (event) => {
      event.preventDefault();
      drop.classList.remove('is-dragging');
    }));
    drop.addEventListener('drop', (event) => {
      if (event.dataTransfer?.files) replaceFiles(event.dataTransfer.files);
    });
  };

  const normalizeCategory = (value) => value
    .trim()
    .replace(/^category:/i, '')
    .replaceAll('_', ' ')
    .replace(/\s+/g, ' ');

  const setupCategoryInput = (input) => {
    const menu = input.parentElement?.querySelector('[data-category-menu]');
    if (!(menu instanceof HTMLElement)) return;
    let timer = 0;
    let requestSequence = 0;
    let abortController;
    let suggestions = [];
    let activeIndex = -1;

    const setActive = (index) => {
      const options = Array.from(menu.querySelectorAll('[role="option"]'));
      options.forEach((option) => option.setAttribute('aria-selected', 'false'));
      if (options.length === 0) {
        activeIndex = -1;
        input.removeAttribute('aria-activedescendant');
        return;
      }
      activeIndex = Math.max(0, Math.min(index, options.length - 1));
      const option = options[activeIndex];
      option.setAttribute('aria-selected', 'true');
      input.setAttribute('aria-activedescendant', option.id);
      option.scrollIntoView({ block: 'nearest' });
    };

    const hide = () => {
      menu.hidden = true;
      input.setAttribute('aria-expanded', 'false');
      input.removeAttribute('aria-activedescendant');
      activeIndex = -1;
    };
    const choose = (category) => {
      input.value = category;
      hide();
      input.focus();
    };
    const render = (categories, failed = false) => {
      menu.replaceChildren();
      suggestions = categories;
      activeIndex = -1;
      categories.forEach((category, index) => {
        const option = document.createElement('div');
        option.className = 'category-option';
        option.id = `${menu.id}-option-${index}`;
        option.setAttribute('role', 'option');
        option.setAttribute('aria-selected', 'false');
        option.textContent = category;
        option.addEventListener('pointerdown', (event) => event.preventDefault());
        option.addEventListener('pointermove', () => setActive(index));
        option.addEventListener('click', () => choose(category));
        menu.append(option);
      });
      if (failed) {
        const note = document.createElement('span');
        note.className = 'category-note';
        note.setAttribute('role', 'status');
        note.textContent = 'Поиск Commons временно недоступен';
        menu.append(note);
      }
      menu.hidden = menu.childElementCount === 0;
      input.setAttribute('aria-expanded', String(!menu.hidden));
    };
    const search = async () => {
      const query = normalizeCategory(input.value);
      const sequence = ++requestSequence;
      abortController?.abort();
      if (query.length < 2) {
        hide();
        return;
      }
      abortController = new AbortController();
      try {
        const response = await fetch(`/api/categories?q=${encodeURIComponent(query)}`, {
          headers: { Accept: 'application/json' },
          signal: abortController.signal,
        });
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        const data = await response.json();
        if (sequence !== requestSequence) return;
        render(Array.isArray(data.categories) ? data.categories : []);
      } catch (error) {
        if (error.name !== 'AbortError' && sequence === requestSequence) render([], true);
      }
    };
    input.addEventListener('input', () => {
      window.clearTimeout(timer);
      timer = window.setTimeout(search, 250);
    });
    input.addEventListener('focus', () => {
      if (normalizeCategory(input.value).length >= 2) search();
    });
    input.addEventListener('blur', () => window.setTimeout(hide, 120));
    input.addEventListener('keydown', (event) => {
      if (event.key === 'Escape') {
        hide();
      } else if (!menu.hidden && event.key === 'ArrowDown') {
        event.preventDefault();
        setActive(activeIndex + 1);
      } else if (!menu.hidden && event.key === 'ArrowUp') {
        event.preventDefault();
        setActive(activeIndex <= 0 ? suggestions.length - 1 : activeIndex - 1);
      } else if (!menu.hidden && event.key === 'Home') {
        event.preventDefault();
        setActive(0);
      } else if (!menu.hidden && event.key === 'End') {
        event.preventDefault();
        setActive(suggestions.length - 1);
      } else if (!menu.hidden && event.key === 'Enter' && activeIndex >= 0) {
        event.preventDefault();
        choose(suggestions[activeIndex]);
      }
    });
  };

  const loadNearby = async (container) => {
    const url = container.dataset.nearbyUrl;
    const item = container.closest('[data-upload-id]');
    const input = item?.querySelector('input[name="category"]');
    if (!url || !(input instanceof HTMLInputElement)) return;
    try {
      const response = await fetch(url, { headers: { Accept: 'application/json' } });
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const data = await response.json();
      container.replaceChildren();
      const categories = Array.isArray(data.categories) ? data.categories : [];
      if (categories.length === 0) {
        const note = document.createElement('span');
        note.className = 'muted';
        note.textContent = 'Поблизости категории не найдены.';
        container.append(note);
        return;
      }
      categories.forEach((candidate) => {
        const button = document.createElement('button');
        button.type = 'button';
        button.className = 'nearby-category';
        const distance = Number.isFinite(candidate.distance_km)
          ? ` · ${candidate.distance_km.toFixed(1)} км`
          : '';
        button.textContent = `${candidate.category}${distance}`;
        button.title = candidate.label
          ? `${candidate.label}${distance}`
          : candidate.category;
        button.addEventListener('click', () => {
          input.value = candidate.category;
          input.focus();
        });
        container.append(button);
      });
    } catch {
      container.replaceChildren();
      const note = document.createElement('span');
      note.className = 'muted';
      note.textContent = 'Не удалось загрузить ближайшие категории.';
      container.append(note);
    }
  };

  const setupNearby = () => {
    const containers = Array.from(document.querySelectorAll('[data-nearby-url]'));
    if (!('IntersectionObserver' in window)) {
      containers.forEach(loadNearby);
      return;
    }
    const observer = new IntersectionObserver((entries) => {
      entries.forEach((entry) => {
        if (!entry.isIntersecting) return;
        observer.unobserve(entry.target);
        loadNearby(entry.target);
      });
    }, { rootMargin: '300px' });
    containers.forEach((container) => observer.observe(container));
  };

  document.querySelectorAll('[data-category-input]').forEach((input) => {
    if (input instanceof HTMLInputElement) setupCategoryInput(input);
  });
  document.querySelectorAll('[data-delete-form]').forEach((form) => {
    form.addEventListener('submit', (event) => {
      if (!window.confirm('Удалить этот файл без возможности восстановления?')) {
        event.preventDefault();
      }
    });
  });
  const uploadForm = document.querySelector('[data-upload-form]');
  uploadForm?.addEventListener('submit', () => {
    const button = uploadForm.querySelector('[data-submit]');
    const status = uploadForm.querySelector('[data-submit-status]');
    if (button instanceof HTMLButtonElement) button.disabled = true;
    if (status) status.textContent = 'Файлы загружаются…';
  });
  setupFiles();
  setupNearby();
})();
