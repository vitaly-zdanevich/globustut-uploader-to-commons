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
		const error = input.form?.querySelector('[data-file-error]');
		const readLimit = (value) => {
			const parsed = Number(value);
			return Number.isSafeInteger(parsed) && parsed > 0 ? parsed : Number.POSITIVE_INFINITY;
		};
		const maxFiles = readLimit(input.dataset.maxFiles);
		const maxFileBytes = readLimit(input.dataset.maxFileBytes);
		const maxRequestBytes = readLimit(input.dataset.maxRequestBytes);

		let files = [];
		let currentError = '';
		const syncInput = () => {
			const transfer = new DataTransfer();
			files.forEach((file) => transfer.items.add(file));
			input.files = transfer.files;
		};
		const setError = (message) => {
			currentError = message;
			input.setCustomValidity(message);
			if (error) {
				error.textContent = message;
				error.hidden = !message;
			}
		};
		const validate = (incoming) => {
			if (incoming.length > maxFiles) {
				return `Можна выбраць не больш за ${maxFiles} файлаў.`;
			}
			const oversized = incoming.find((file) => file.size > maxFileBytes);
			if (oversized) {
				return `Файл «${oversized.name}» перавышае ліміт ${formatSize(maxFileBytes)}.`;
			}
			const totalBytes = incoming.reduce((total, file) => total + file.size, 0);
			if (totalBytes > maxRequestBytes) {
				return `Агульны памер файлаў перавышае ${formatSize(maxRequestBytes)}.`;
			}
			return '';
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
				remove.title = `Выдаліць ${file.name}`;
				remove.setAttribute('aria-label', `Выдаліць ${file.name}`);
				remove.addEventListener('click', () => {
					files.splice(index, 1);
					syncInput();
					render();
					setError('');
				});
				row.append(name, size, remove);
				list.append(row);
			});
		};
		const replaceFiles = (incoming) => {
			const nextFiles = Array.from(incoming);
			const validationError = validate(nextFiles);
			if (validationError) {
				files = [];
				syncInput();
				render();
				setError(validationError);
				return;
			}
			files = nextFiles;
			syncInput();
			render();
			setError('');
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
		input.form?.addEventListener('submit', (event) => {
			const validationError = currentError || validate(Array.from(input.files ?? []));
			if (!validationError) return;
			event.preventDefault();
			setError(validationError);
			input.focus();
		});
	};

	const normalizeCategory = (value) => value
		.trim()
		.replace(/^category:/i, '')
		.replaceAll('_', ' ')
		.replace(/\s+/g, ' ');

	const categoryControllers = new WeakMap();

	const commonsCategoryUrl = (category) => (
		`https://commons.wikimedia.org/wiki/Category:${encodeURIComponent(category.replaceAll(' ', '_'))}`
	);

	const setupCategoryAutocomplete = (input, onCommit) => {
		const menu = input.parentElement?.querySelector('[data-category-menu]');
		if (!(menu instanceof HTMLElement)) return null;

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

		const cancelSearch = () => {
			window.clearTimeout(timer);
			timer = 0;
			requestSequence += 1;
			abortController?.abort();
			abortController = undefined;
		};
		const commit = (category) => {
			const normalized = normalizeCategory(category);
			if (!normalized) return false;
			cancelSearch();
			hide();
			onCommit(normalized);
			return true;
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
				option.addEventListener('click', () => commit(category));
				menu.append(option);
			});
			if (failed) {
				const note = document.createElement('span');
				note.className = 'category-note';
				note.setAttribute('role', 'status');
				note.textContent = 'Пошук у Commons часова недаступны';
				menu.append(note);
			}
			menu.hidden = menu.childElementCount === 0;
			input.setAttribute('aria-expanded', String(!menu.hidden));
			if (categories.length > 0) setActive(0);
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
				cancelSearch();
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
			} else if (event.key === 'Enter' && !event.isComposing) {
				const typedCategory = normalizeCategory(input.value);
				if (!typedCategory) return;
				event.preventDefault();
				if (!menu.hidden && suggestions.length > 0) {
					commit(suggestions[activeIndex >= 0 ? activeIndex : 0]);
				} else {
					commit(typedCategory);
				}
			}
		});
		return { cancelSearch, commit, hide };
	};

	const setupSingleCategoryInput = (input) => {
		const chip = document.createElement('span');
		chip.className = 'category-chip';
		chip.dataset.categoryChip = '';
		chip.hidden = true;
		const chipLabel = document.createElement('span');
		chipLabel.className = 'category-chip-label';
		const chipActions = document.createElement('span');
		chipActions.className = 'category-chip-actions';
		const chipLink = document.createElement('a');
		chipLink.className = 'category-chip-link';
		const linkEmoji = document.createElement('span');
		linkEmoji.textContent = '🔗';
		linkEmoji.setAttribute('aria-hidden', 'true');
		chipLink.append(linkEmoji);
		chipLink.target = '_blank';
		chipLink.rel = 'noopener';
		const remove = document.createElement('button');
		remove.type = 'button';
		remove.className = 'category-chip-remove';
		remove.textContent = '×';
		chipActions.append(chipLink, remove);
		chip.append(chipLabel, chipActions);
		input.before(chip);

		const apply = (category, focusRemove = true) => {
			const normalized = normalizeCategory(category);
			if (!normalized) return false;
			input.value = normalized;
			chipLabel.textContent = normalized;
			const linkLabel = `Адкрыць катэгорыю ${normalized} у Wikimedia Commons`;
			chipLink.href = commonsCategoryUrl(normalized);
			chipLink.title = linkLabel;
			chipLink.setAttribute('aria-label', linkLabel);
			remove.title = `Выдаліць катэгорыю ${normalized}`;
			remove.setAttribute('aria-label', `Выдаліць катэгорыю ${normalized}`);
			input.hidden = true;
			chip.hidden = false;
			if (focusRemove) remove.focus();
			return true;
		};
		const autocomplete = setupCategoryAutocomplete(input, apply);
		if (!autocomplete) return;
		const clear = () => {
			autocomplete.cancelSearch();
			autocomplete.hide();
			chip.hidden = true;
			input.hidden = false;
			input.value = '';
			input.focus();
		};
		remove.addEventListener('click', clear);
		categoryControllers.set(input, { commit: apply });
		if (normalizeCategory(input.value)) apply(input.value, false);
	};

	const setupCategoryEditor = (editor) => {
		const item = editor.closest('[data-upload-id]');
		const list = editor.querySelector('[data-category-list]');
		const form = editor.querySelector('[data-category-add-form]');
		const input = form?.querySelector('[data-category-input]');
		const csrf = form?.querySelector('input[name="csrf_token"]');
		const status = editor.querySelector('[data-category-status]');
		if (
			!(item instanceof HTMLElement)
			|| !(list instanceof HTMLElement)
			|| !(form instanceof HTMLFormElement)
			|| !(input instanceof HTMLInputElement)
			|| !(csrf instanceof HTMLInputElement)
		) return;

		let categories = Array.from(list.querySelectorAll('[data-category-value]'))
			.map((chip) => normalizeCategory(chip.dataset.categoryValue ?? ''))
			.filter(Boolean);
		let busy = false;

		const setStatus = (message, error = false) => {
			if (!status) return;
			status.textContent = message;
			status.classList.toggle('is-error', error);
		};
		const setBusy = (nextBusy) => {
			busy = nextBusy;
			input.disabled = nextBusy;
			list.querySelectorAll('button').forEach((button) => {
				button.disabled = nextBusy || categories.length <= 1;
			});
		};
		const persist = async (url, category) => {
			if (busy) return null;
			setBusy(true);
			setStatus('Захоўваем катэгорыі…');
			try {
				const body = new URLSearchParams({
					csrf_token: csrf.value,
					category,
				});
				const response = await fetch(url, {
					method: 'POST',
					headers: {
						Accept: 'application/json',
						'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8',
					},
					body,
				});
				if (!response.ok) throw new Error(`HTTP ${response.status}`);
				const data = await response.json();
				if (!Array.isArray(data.categories) || data.categories.length === 0) {
					throw new Error('Invalid categories response');
				}
				return data.categories.map(normalizeCategory).filter(Boolean);
			} catch {
				setStatus('Не ўдалося захаваць катэгорыі.', true);
				return null;
			} finally {
				setBusy(false);
			}
		};
		const render = () => {
			list.replaceChildren();
			categories.forEach((category) => {
				const chip = document.createElement('li');
				chip.className = 'category-chip';
				chip.dataset.categoryChip = '';
				chip.dataset.categoryValue = category;
				const label = document.createElement('span');
				label.className = 'category-chip-label';
				label.textContent = category;
				const actions = document.createElement('span');
				actions.className = 'category-chip-actions';
				const link = document.createElement('a');
				link.className = 'category-chip-link';
				link.href = commonsCategoryUrl(category);
				link.target = '_blank';
				link.rel = 'noopener noreferrer';
				const linkLabel = `Адкрыць катэгорыю ${category} у Wikimedia Commons`;
				link.title = linkLabel;
				link.setAttribute('aria-label', linkLabel);
				const emoji = document.createElement('span');
				emoji.textContent = '🔗';
				emoji.setAttribute('aria-hidden', 'true');
				link.append(emoji);
				const remove = document.createElement('button');
				remove.type = 'button';
				remove.className = 'category-chip-remove';
				remove.textContent = '×';
				remove.title = `Выдаліць катэгорыю ${category}`;
				remove.setAttribute('aria-label', `Выдаліць катэгорыю ${category}`);
				remove.disabled = busy || categories.length <= 1;
				remove.addEventListener('click', async () => {
					if (categories.length <= 1) {
						setStatus('Файл павінен мець хаця б адну катэгорыю. Спачатку дадайце новую.', true);
						return;
					}
					const updated = await persist(
						`/admin/uploads/${encodeURIComponent(item.dataset.uploadId ?? '')}/categories/remove`,
						category,
					);
					if (!updated) return;
					categories = updated;
					render();
					setStatus('Катэгорыя выдалена.');
					input.focus();
				});
				actions.append(link, remove);
				chip.append(label, actions);
				list.append(chip);
			});
			setBusy(busy);
		};
		const add = async (rawCategory) => {
			const category = normalizeCategory(rawCategory);
			if (!category) return false;
			if (categories.includes(category)) {
				input.value = '';
				setStatus('Катэгорыя ўжо дададзена.');
				input.focus();
				return true;
			}
			const updated = await persist(form.action, category);
			if (!updated) return false;
			categories = updated;
			render();
			input.value = '';
			setStatus('Катэгорыя дададзена.');
			input.focus();
			return true;
		};
		const autocomplete = setupCategoryAutocomplete(input, (category) => {
			void add(category);
		});
		if (!autocomplete) return;
		form.addEventListener('submit', (event) => {
			event.preventDefault();
			autocomplete.commit(input.value);
		});
		categoryControllers.set(input, { commit: autocomplete.commit });
		render();
	};

	const loadNearby = async (container) => {
		const url = container.dataset.nearbyUrl;
		const item = container.closest('[data-upload-id]');
		const input = item?.querySelector(
			'[data-category-editor] [data-category-add-form] [data-category-input]',
		);
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
				note.textContent = 'Найбліжэйшыя катэгорыі не знойдзены.';
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
					void categoryControllers.get(input)?.commit(candidate.category);
				});
				container.append(button);
			});
		} catch {
			container.replaceChildren();
			const note = document.createElement('span');
			note.className = 'muted';
			note.textContent = 'Не ўдалося запампаваць найбліжэйшыя катэгорыі.';
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

	const setupImageHover = () => {
		const finePointer = window.matchMedia('(hover: hover) and (pointer: fine)');
		if (!finePointer.matches) return;
		const links = Array.from(document.querySelectorAll('.media-stage a[href]'))
			.filter((link) => link.querySelector('img') instanceof HTMLImageElement);
		if (links.length === 0) return;

		const overlay = document.createElement('div');
		overlay.dataset.imageHoverOverlay = '';
		overlay.setAttribute('aria-hidden', 'true');
		overlay.hidden = true;
		const image = document.createElement('img');
		image.alt = '';
		overlay.append(image);
		document.body.append(overlay);
		let activeLink;
		let requestSequence = 0;

		const hide = (link) => {
			if (link && activeLink !== link) return;
			requestSequence += 1;
			activeLink = undefined;
			overlay.hidden = true;
			image.alt = '';
		};
		const show = (link) => {
			const thumbnail = link.querySelector('img');
			if (!(thumbnail instanceof HTMLImageElement)) return;
			const sequence = ++requestSequence;
			activeLink = link;
			image.src = thumbnail.currentSrc || thumbnail.src;
			image.alt = thumbnail.alt;
			overlay.hidden = false;
			const original = new Image();
			original.addEventListener('load', () => {
				if (sequence === requestSequence && activeLink === link) {
					image.src = original.src;
				}
			});
			original.src = link.href;
		};
		links.forEach((link) => {
			link.addEventListener('pointerenter', (event) => {
				if (event.pointerType === 'touch' || !finePointer.matches) return;
				show(link);
			});
			link.addEventListener('pointerleave', () => hide(link));
			link.addEventListener('pointercancel', () => hide(link));
			link.addEventListener('click', () => hide(link));
		});
		window.addEventListener('blur', () => hide());
		window.addEventListener('scroll', () => hide(), { passive: true });
		document.addEventListener('keydown', (event) => {
			if (event.key === 'Escape') hide();
		});
	};

	const setupPublisher = () => {
		const form = document.querySelector('[data-publish-form]');
		if (!(form instanceof HTMLFormElement)) return;
		const submit = form.querySelector('[data-publish-submit]');
		const spinner = form.querySelector('[data-publish-spinner]');
		const status = form.querySelector('[data-publish-status]');
		const results = document.querySelector('[data-publish-results]');
		const csrf = form.querySelector('input[name="csrf_token"]');
		const queueStats = document.querySelector('[data-queue-stats]');
		const paginationSlot = document.querySelector('[data-pagination-slot]');
		if (
			!(submit instanceof HTMLButtonElement)
			|| !(spinner instanceof HTMLElement)
			|| !(status instanceof HTMLElement)
			|| !(results instanceof HTMLElement)
			|| !(csrf instanceof HTMLInputElement)
		) return;

		const pendingFields = () => Array.from(
			form.querySelectorAll('input[data-publish-upload-id]'),
		).filter((field) => field instanceof HTMLInputElement);
		const normalizeNativeField = () => {
			pendingFields().forEach((field, index) => {
				if (index === 0) {
					field.name = 'upload_id';
				} else {
					field.removeAttribute('name');
				}
			});
		};
		const queueNumber = (value) => {
			const parsed = Number(value);
			return Number.isSafeInteger(parsed) && parsed >= 0 ? parsed : 0;
		};
		const updateQueueSummary = (publishedCount, publishedBytes) => {
			if (publishedCount === 0) return;
			const totalCount = queueStats instanceof HTMLElement
				? queueNumber(queueStats.dataset.totalCount)
				: 0;
			const storedBytes = queueStats instanceof HTMLElement
				? queueNumber(queueStats.dataset.storedBytes)
				: 0;
			const storageLimitBytes = queueStats instanceof HTMLElement
				? queueNumber(queueStats.dataset.storageLimitBytes)
				: 0;
			const storageLimitLabel = queueStats instanceof HTMLElement
				? queueStats.dataset.storageLimitLabel
				: '';
			const remainingCount = Math.max(0, totalCount - publishedCount);
			const remainingBytes = Math.max(0, storedBytes - publishedBytes);
			if (queueStats instanceof HTMLElement) {
				queueStats.dataset.totalCount = String(remainingCount);
				queueStats.dataset.storedBytes = String(remainingBytes);
				const count = queueStats.querySelector('[data-queue-count]');
				const files = queueStats.querySelector('[data-queue-files]');
				const disk = queueStats.querySelector('[data-queue-disk]');
				if (count) count.textContent = String(remainingCount);
				if (files) files.textContent = formatSize(remainingBytes);
				if (disk) {
					const mebibyte = 1024 * 1024;
					const fallbackLimit = `${Math.floor(storageLimitBytes / mebibyte)} МБ`;
					disk.textContent = `${Math.floor(remainingBytes / mebibyte)} МБ / ${storageLimitLabel || fallbackLimit}`;
				}
			}

			const visibleCount = document.querySelectorAll('.moderation-item').length;
			if (paginationSlot instanceof HTMLElement) {
				paginationSlot.replaceChildren();
				if (remainingCount > visibleCount) {
					const navigation = document.createElement('nav');
					navigation.className = 'pagination';
					navigation.setAttribute('aria-label', 'Старонкі');
					const continueLink = document.createElement('a');
					continueLink.className = 'page-button';
					continueLink.href = '/admin';
					continueLink.textContent = 'Працягнуць мадэрацыю';
					navigation.append(continueLink);
					paginationSlot.append(navigation);
				}
			}

			const list = document.querySelector('.moderation-list');
			if (
				remainingCount === 0
				&& list instanceof HTMLElement
				&& !list.querySelector('.moderation-item, .empty-state')
			) {
				const empty = document.createElement('div');
				empty.className = 'empty-state';
				const heading = document.createElement('h2');
				heading.textContent = 'Чарга пустая';
				const note = document.createElement('p');
				note.textContent = "Новыя матэрыялы з'явяцца тут пасля адпраўкі формы.";
				empty.append(heading, note);
				list.append(empty);
			}
		};
		const resultRow = (uploadId) => {
			const existing = Array.from(results.querySelectorAll('[data-publish-result-id]'))
				.find((row) => row.dataset.publishResultId === uploadId);
			if (existing instanceof HTMLElement) return existing;
			const row = document.createElement('div');
			row.className = 'publish-result';
			row.dataset.publishResultId = uploadId;
			results.append(row);
			return row;
		};
		const renderSuccess = (uploadId, data) => {
			const row = resultRow(uploadId);
			row.className = 'publish-result is-success';
			row.replaceChildren();
			const sourceName = document.createElement('span');
			sourceName.className = 'publish-result-name';
			sourceName.textContent = data.display_filename;
			const links = document.createElement('span');
			links.className = 'publish-result-links';
			const pageLink = document.createElement('a');
			pageLink.href = data.page_url;
			pageLink.target = '_blank';
			pageLink.rel = 'noopener noreferrer';
			pageLink.textContent = data.filename;
			links.append(pageLink);
			if (typeof data.file_url === 'string' && data.file_url) {
				const directLink = document.createElement('a');
				directLink.href = data.file_url;
				directLink.target = '_blank';
				directLink.rel = 'noopener noreferrer';
				directLink.textContent = 'Прамы файл';
				links.append(directLink);
			}
			row.append(sourceName, links);
		};
		const renderError = (uploadId, message) => {
			const row = resultRow(uploadId);
			row.className = 'publish-result is-error';
			row.replaceChildren();
			const item = document.getElementById(`upload-${uploadId}`);
			const preview = item?.querySelector('.media-stage img');
			const name = document.createElement('span');
			name.className = 'publish-result-name';
			name.textContent = preview instanceof HTMLImageElement ? preview.alt : uploadId;
			const detail = document.createElement('span');
			detail.className = 'publish-result-error';
			detail.textContent = message;
			row.append(name, detail);
		};
		const publishOne = async (uploadId) => {
			const response = await fetch(form.action, {
				method: 'POST',
				headers: {
					Accept: 'application/json',
					'Content-Type': 'application/x-www-form-urlencoded;charset=UTF-8',
				},
				body: new URLSearchParams({
					csrf_token: csrf.value,
					upload_id: uploadId,
				}),
			});
			if (!response.ok) {
				const message = response.status === 409
					? 'Файл з такой назвай ужо існуе ў Wikimedia Commons.'
					: response.status === 423
						? 'З гэтым файлам ужо выконваецца іншая аперацыя.'
						: `Не ўдалося запампаваць файл (HTTP ${response.status}).`;
				return {
					error: message,
					stop: [401, 403, 429, 502, 503, 504].includes(response.status),
				};
			}
			const data = await response.json();
			if (
				typeof data.display_filename !== 'string'
				|| typeof data.filename !== 'string'
				|| typeof data.page_url !== 'string'
			) throw new Error('Invalid publish response');
			return { data };
		};

		form.addEventListener('submit', async (event) => {
			event.preventDefault();
			const fields = pendingFields();
			if (fields.length === 0 || form.getAttribute('aria-busy') === 'true') return;
			submit.disabled = true;
			spinner.hidden = false;
			form.setAttribute('aria-busy', 'true');
			let succeeded = 0;
			let publishedBytes = 0;
			let failed = 0;
			let stopped = false;

			for (const [index, field] of fields.entries()) {
				const uploadId = field.value;
				const uploadSize = queueNumber(field.dataset.publishSizeBytes);
				const item = document.getElementById(`upload-${uploadId}`);
				const preview = item?.querySelector('.media-stage img');
				const displayName = preview instanceof HTMLImageElement ? preview.alt : uploadId;
				status.textContent = `Запампоўваецца ${index + 1} з ${fields.length}: ${displayName}`;
				try {
					const result = await publishOne(uploadId);
					if (result.error) {
						failed += 1;
						renderError(uploadId, result.error);
						if (result.stop) {
							stopped = true;
							break;
						}
						continue;
					}
					succeeded += 1;
					publishedBytes += uploadSize;
					renderSuccess(uploadId, result.data);
					field.remove();
					document.getElementById(`upload-${uploadId}`)?.remove();
				} catch {
					failed += 1;
					stopped = true;
					renderError(uploadId, 'Не ўдалося звязацца з серверам.');
					break;
				}
			}

			spinner.hidden = true;
			form.removeAttribute('aria-busy');
			normalizeNativeField();
			updateQueueSummary(succeeded, publishedBytes);
			submit.disabled = pendingFields().length === 0;
			if (stopped) {
				status.textContent = `Запампоўванне спынена. Запампавана: ${succeeded}. Памылак: ${failed}.`;
			} else if (failed > 0) {
				status.textContent = `Запампавана: ${succeeded}. Памылак: ${failed}.`;
			} else {
				status.textContent = `Запампавана файлаў: ${succeeded}.`;
			}
		});
	};

	document.querySelectorAll('[data-category-editor]').forEach((editor) => {
		if (editor instanceof HTMLElement) setupCategoryEditor(editor);
	});
	document.querySelectorAll('[data-category-input]').forEach((input) => {
		if (
			input instanceof HTMLInputElement
			&& !input.closest('[data-category-editor]')
		) setupSingleCategoryInput(input);
	});
	document.querySelectorAll('[data-delete-form]').forEach((form) => {
		form.addEventListener('submit', (event) => {
			if (!window.confirm('Выдаліць гэты файл без магчымасці аднаўлення?')) {
				event.preventDefault();
			}
		});
	});
	setupFiles();
	const uploadForm = document.querySelector('[data-upload-form]');
	uploadForm?.addEventListener('submit', (event) => {
		if (event.defaultPrevented) return;
		const button = uploadForm.querySelector('[data-submit]');
		const status = uploadForm.querySelector('[data-submit-status]');
		if (button instanceof HTMLButtonElement) button.disabled = true;
		if (status) status.textContent = 'Файлы запампоўваюцца…';
	});
	setupNearby();
	setupImageHover();
	setupPublisher();
})();
