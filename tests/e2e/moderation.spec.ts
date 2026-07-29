import { expect, test, type Locator, type Page } from '@playwright/test';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

test.describe.configure({ mode: 'serial' });

const moderatorUsername = 'moderator';
const moderatorPassword = 'correct horse battery staple';

let fixtureDirectory = '';
let gpsPhoto = '';
let plainPhoto = '';

test.beforeAll(() => {
  fixtureDirectory = mkdtempSync(join(tmpdir(), 'globustut-playwright-'));
  gpsPhoto = join(fixtureDirectory, 'castle.jpg');
  plainPhoto = join(fixtureDirectory, 'plain.jpg');
  const imageMagick = spawnSync('magick', ['-version']).status === 0 ? 'magick' : 'convert';
  execFileSync(imageMagick, ['-size', '640x480', 'xc:#4cb6ac', gpsPhoto]);
  execFileSync(imageMagick, ['-size', '320x240', 'xc:#d7d9d7', plainPhoto]);
  execFileSync('exiftool', [
    '-overwrite_original',
    '-GPSLatitude=53.9045',
    '-GPSLatitudeRef=N',
    '-GPSLongitude=27.5615',
    '-GPSLongitudeRef=E',
    '-DateTimeOriginal=2025:07:18 12:13:14',
    '-XMP-dc:Creator=Test Photographer',
    gpsPhoto,
  ]);
});

test.afterAll(() => {
  if (fixtureDirectory) rmSync(fixtureDirectory, { recursive: true, force: true });
});

const loginAsModerator = async (page: Page) => {
  await page.goto('/admin');
  if (new URL(page.url()).pathname === '/admin/login') {
    await expect(page.getByRole('heading', { name: 'Уваход' })).toBeVisible();
    await page.getByLabel('Імя карыстальніка').fill(moderatorUsername);
    await page.getByLabel('Пароль').fill(moderatorPassword);
    await page.getByRole('button', { name: 'Увайсці' }).click();
  }
  await expect(page).toHaveURL(/\/admin$/);
};

const clearModerationQueue = async (page: Page) => {
  await loginAsModerator(page);
  const cards = page.locator('.moderation-item');
  for (let remaining = await cards.count(); remaining > 0; remaining = await cards.count()) {
    page.once('dialog', (dialog) => dialog.accept());
    await cards.first().getByRole('button', { name: 'Выдаліць файл' }).click();
    await expect(cards).toHaveCount(remaining - 1);
  }
};

const cardForOriginalFile = (page: Page, originalFilename: string) => (
  page.locator('.moderation-item').filter({ hasText: originalFilename })
);

const categoryChip = (card: Locator, category: string) => (
  card.locator('[data-category-chip]').filter({ hasText: category })
);

const waitForCategoryMutation = (page: Page, operation: 'add' | 'remove') => (
  page.waitForResponse((response) => {
    const path = new URL(response.url()).pathname;
    if (response.request().method() !== 'POST') return false;
    return operation === 'remove'
      ? /\/admin\/uploads\/[^/]+\/categories\/remove$/.test(path)
      : /\/admin\/uploads\/[^/]+\/categories$/.test(path);
  })
);

const expectLoadedNonBlankImage = async (image: Locator) => {
  await expect(image).toBeVisible();
  await expect.poll(() => image.evaluate((element: HTMLImageElement) => (
    element.complete && element.naturalWidth > 0 && element.naturalHeight > 0
  ))).toBe(true);
  const hasVisiblePixels = await image.evaluate(async (element: HTMLImageElement) => {
    await element.decode();
    const canvas = document.createElement('canvas');
    canvas.width = 16;
    canvas.height = 16;
    const context = canvas.getContext('2d');
    if (!context) return false;
    context.drawImage(element, 0, 0, canvas.width, canvas.height);
    const pixels = context.getImageData(0, 0, canvas.width, canvas.height).data;
    for (let offset = 0; offset < pixels.length; offset += 4) {
      if (
        pixels[offset + 3] > 0
        && pixels[offset] + pixels[offset + 1] + pixels[offset + 2] > 0
      ) return true;
    }
    return false;
  });
  expect(hasVisiblePixels).toBe(true);
};

test('Belarusian upload, moderation, category, preview, and publish workflow', async (
  { page },
  testInfo,
) => {
  test.setTimeout(120_000);
  const mobile = testInfo.project.name.startsWith('mobile');
  const viewportName = mobile ? 'mobile' : 'desktop';
  const runId = `${viewportName}-${Date.now()}-${testInfo.retry}`;
  const gpsFilename = `castle-${runId}.jpg`;
  const plainFilename = `plain-${runId}.jpg`;
  const renamedBasename = `Нясвіжскі замак ${runId}`;
  const renamedFilename = `${renamedBasename}.jpg`;

  // A retry or the preceding viewport project may have left pending files.
  await clearModerationQueue(page);

  await page.goto('/?category=Castles_in_Belarus');
  expect(await page.locator('html').getAttribute('lang')).toBe('be');
  await expect(page.getByRole('heading', { name: 'Дадаць фатаграфіі/відэа' })).toBeVisible();
  await expect(page.getByText(/Creative Commons BY 4\.0/)).toBeVisible();
  await expect(page.getByText('Аўтар будзе пазначаны паводле метаданых.')).toBeVisible();
  await expect(page.getByText('Дзякуй за захаванне культурнай спадчыны')).toBeVisible();
  await expect(page.getByText(/Новая (?:публікацыя|публикация)/i)).toHaveCount(0);
  await expect(page.getByText(/Да 500 файлаў.+агульны аб'ём да 2 ГБ/)).toBeVisible();
  await expect(page.locator('input[name=license], input[name=rights_confirmed]')).toHaveCount(0);
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth))
    .toBe(true);

  const publicCategory = page.locator('#upload-category');
  const publicChip = page.locator('[data-category-chip]');
  await expect(publicChip.locator('.category-chip-label')).toHaveText('Castles in Belarus');
  await expect(publicCategory).toBeHidden();
  const publicCategoryLink = publicChip.getByRole('link', {
    name: 'Адкрыць катэгорыю Castles in Belarus у Wikimedia Commons',
  });
  await expect(publicCategoryLink).toHaveAttribute(
    'href',
    'https://commons.wikimedia.org/wiki/Category:Castles_in_Belarus',
  );
  await expect(publicCategoryLink).toHaveAttribute('target', '_blank');
  await expect(publicCategoryLink).toHaveAttribute('rel', 'noopener');
  await publicChip.getByRole('button', {
    name: 'Выдаліць катэгорыю Castles in Belarus',
  }).click();
  await expect(publicCategory).toBeVisible();
  await expect(publicCategory).toBeFocused();
  await expect(publicCategory).toHaveValue('');

  const fileInput = page.locator('input[type=file]');
  const maxFiles = Number(await fileInput.getAttribute('data-max-files'));
  expect(maxFiles).toBe(500);
  await fileInput.setInputFiles({
    name: gpsFilename,
    mimeType: 'image/jpeg',
    buffer: readFileSync(gpsPhoto),
  });
  await expect(page.getByText(gpsFilename)).toBeVisible();
  await fileInput.setInputFiles(Array.from({ length: maxFiles + 1 }, (_, index) => ({
    name: `extra-${runId}-${index}.jpg`,
    mimeType: 'image/jpeg',
    buffer: Buffer.from([0]),
  })));
  await expect(page.getByRole('alert')).toHaveText(
    `Можна выбраць не больш за ${maxFiles} файлаў.`,
  );
  expect(await fileInput.evaluate((element: HTMLInputElement) => element.files?.length)).toBe(0);
  await expect(page.locator('.selected-file')).toHaveCount(0);

  await fileInput.setInputFiles([
    {
      name: gpsFilename,
      mimeType: 'image/jpeg',
      buffer: readFileSync(gpsPhoto),
    },
    {
      name: plainFilename,
      mimeType: 'image/jpeg',
      buffer: readFileSync(plainPhoto),
    },
  ]);
  await expect(page.getByRole('alert')).toBeHidden();
  await expect(page.getByText(gpsFilename)).toBeVisible();
  await expect(page.getByText(plainFilename)).toBeVisible();
  await publicCategory.fill('Castl');
  await expect(page.getByRole('option', { name: 'Castles in Belarus' })).toBeVisible();
  await publicCategory.press('Enter');
  await expect(publicCategory).toHaveValue('Castles in Belarus');
  await expect(publicCategory).toBeHidden();
  await expect(publicChip.locator('.category-chip-label')).toHaveText('Castles in Belarus');
  await page.getByRole('button', { name: 'Адправіць на прэмадэрацыю' }).click();
  await expect(page.getByRole('heading', { name: 'Прынята файлаў: 2' })).toBeVisible();

  await page.goto('/admin');
  await expect(page).toHaveURL(/\/admin$/);
  await expect(page.locator('.moderation-item')).toHaveCount(2);
  await expect(page.locator('.queue-title, .queue-heading h1')).toHaveCount(0);
  await expect(page.getByRole('heading', { name: 'Прэмадэрацыя' })).toHaveCount(0);
  await expect(page.getByText('Запампаваныя файлы')).toBeVisible();

  let gpsCard = cardForOriginalFile(page, gpsFilename);
  let plainCard = cardForOriginalFile(page, plainFilename);
  await expect(gpsCard).toBeVisible();
  await expect(plainCard).toBeVisible();

  const preview = gpsCard.locator('.media-stage img');
  const mediaLink = gpsCard.locator('.media-stage a');
  await expectLoadedNonBlankImage(preview);
  if (mobile) {
    await expect(page.locator('[data-image-hover-overlay]')).toHaveCount(0);
    const popupPromise = page.waitForEvent('popup');
    await mediaLink.click();
    const popup = await popupPromise;
    await expect(popup).toHaveURL(/\/admin\/media\/[^/]+$/);
    await popup.close();
    await expect(page.locator('[data-image-hover-overlay]')).toHaveCount(0);
  } else {
    const overlay = page.locator('[data-image-hover-overlay]');
    await expect(overlay).toHaveCount(1);
    await page.keyboard.press('Escape');
    await expect(overlay).toBeHidden();
    await mediaLink.scrollIntoViewIfNeeded();
    await page.mouse.move(0, 0);
    await page.waitForTimeout(100);
    await mediaLink.hover();
    await expect(overlay).toBeVisible();
    const overlayImage = overlay.locator('img');
    const mediaUrl = await mediaLink.evaluate((element: HTMLAnchorElement) => element.href);
    await expect(overlayImage).toHaveAttribute('src', mediaUrl);
    await expectLoadedNonBlankImage(overlayImage);
    const bounds = await overlay.evaluate((element: HTMLElement) => {
      const rectangle = element.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        background: style.backgroundColor,
        height: rectangle.height,
        left: rectangle.left,
        position: style.position,
        top: rectangle.top,
        width: rectangle.width,
      };
    });
    await expect(overlayImage).toHaveCSS('object-fit', 'contain');
    const viewport = page.viewportSize();
    expect(viewport).not.toBeNull();
    expect(bounds.position).toBe('fixed');
    expect(bounds.background).toBe('rgb(0, 0, 0)');
    expect(bounds.left).toBeCloseTo(0, 0);
    expect(bounds.top).toBeCloseTo(0, 0);
    expect(bounds.width).toBeGreaterThanOrEqual((viewport?.width ?? 0) - 1);
    expect(bounds.height).toBeGreaterThanOrEqual((viewport?.height ?? 0) - 1);
    await page.keyboard.press('Escape');
    await expect(overlay).toBeHidden();
  }

  await expect(gpsCard.getByRole('link', { name: /OpenStreetMap/ })).toHaveAttribute(
    'href',
    /mlat=53\.904500&mlon=27\.561500/,
  );
  await expect(gpsCard.getByText('Test Photographer', { exact: true })).toBeVisible();
  expect(await gpsCard.locator('.file-meta dt').allTextContents())
    .toEqual(['Памер', 'Аўтар', 'Месца', 'Зыходнае імя']);
  await expect(gpsCard.getByText('2025-07-18')).toHaveCount(0);
  await expect(gpsCard.getByText('CC BY 4.0')).toHaveCount(0);
  await expect(gpsCard.getByText(/^(?:Фото|Фота)$/)).toHaveCount(0);
  await expect(gpsCard.getByText(/^(?:Съёмка|Здымка|Ліцэнзія)$/)).toHaveCount(0);

  await gpsCard.locator('input[name=basename]').fill(renamedBasename);
  await gpsCard.getByRole('button', { name: 'Захаваць імя' }).click();
  gpsCard = cardForOriginalFile(page, gpsFilename);
  await expect(gpsCard.locator('input[name=basename]')).toHaveValue(renamedBasename);

  const categoryStatus = gpsCard.locator('[data-category-status]');
  const initialChip = categoryChip(gpsCard, 'Castles in Belarus');
  await expect(initialChip).toHaveCount(1);
  await expect(initialChip.getByRole('button', {
    name: 'Выдаліць катэгорыю Castles in Belarus',
  })).toBeDisabled();
  await expect(initialChip.getByRole('link', {
    name: 'Адкрыць катэгорыю Castles in Belarus у Wikimedia Commons',
  })).toHaveAttribute(
    'href',
    'https://commons.wikimedia.org/wiki/Category:Castles_in_Belarus',
  );

  await gpsCard.scrollIntoViewIfNeeded();
  const nearby = gpsCard.locator('.nearby-category');
  await expect(nearby).toHaveCount(20);
  await expect(nearby.first()).toHaveText('Nearby category 25 · 0.1 км');
  const [nearbyResponse] = await Promise.all([
    waitForCategoryMutation(page, 'add'),
    nearby.first().click(),
  ]);
  expect(nearbyResponse.ok()).toBe(true);
  await expect(categoryStatus).toHaveText('Катэгорыя дададзена.');
  const nearbyChip = categoryChip(gpsCard, 'Nearby category 25');
  await expect(nearbyChip).toHaveCount(1);
  await expect(nearbyChip.getByRole('link', {
    name: 'Адкрыць катэгорыю Nearby category 25 у Wikimedia Commons',
  })).toHaveAttribute(
    'href',
    'https://commons.wikimedia.org/wiki/Category:Nearby_category_25',
  );

  const categoryInput = gpsCard.getByRole('combobox', {
    name: 'Дадаць катэгорыю Wikimedia Commons',
  });
  await categoryInput.fill('Castl');
  const autocompleteOption = gpsCard.getByRole('option', { name: 'Castles of Minsk' });
  await expect(autocompleteOption).toBeVisible();
  const [autocompleteResponse] = await Promise.all([
    waitForCategoryMutation(page, 'add'),
    autocompleteOption.click(),
  ]);
  expect(autocompleteResponse.ok()).toBe(true);
  await expect(categoryStatus).toHaveText('Катэгорыя дададзена.');
  await expect(categoryChip(gpsCard, 'Castles of Minsk')).toHaveCount(1);
  await expect(gpsCard.locator('[data-category-chip]')).toHaveCount(3);

  const [removeResponse] = await Promise.all([
    waitForCategoryMutation(page, 'remove'),
    initialChip.getByRole('button', {
      name: 'Выдаліць катэгорыю Castles in Belarus',
    }).click(),
  ]);
  expect(removeResponse.ok()).toBe(true);
  await expect(categoryStatus).toHaveText('Катэгорыя выдалена.');
  await expect(categoryChip(gpsCard, 'Castles in Belarus')).toHaveCount(0);
  await expect(gpsCard.locator('[data-category-chip]')).toHaveCount(2);

  await page.reload();
  gpsCard = cardForOriginalFile(page, gpsFilename);
  plainCard = cardForOriginalFile(page, plainFilename);
  await expect(gpsCard.locator('[data-category-list] .category-chip-label')).toHaveText([
    'Nearby category 25',
    'Castles of Minsk',
  ]);
  await expect(categoryChip(gpsCard, 'Castles in Belarus')).toHaveCount(0);
  for (const category of ['Nearby category 25', 'Castles of Minsk']) {
    await expect(categoryChip(gpsCard, category).getByRole('link', {
      name: `Адкрыць катэгорыю ${category} у Wikimedia Commons`,
    })).toHaveAttribute(
      'href',
      `https://commons.wikimedia.org/wiki/Category:${category.replaceAll(' ', '_')}`,
    );
  }

  await expect(
    plainCard.getByText(
      'Для пошуку найбліжэйшых катэгорый патрэбныя каардынаты EXIF.',
    ),
  ).toBeVisible();
  await expect(plainCard.locator('[data-nearby-url]')).toHaveCount(0);
  expect(await plainCard.locator('.file-meta dt').allTextContents())
    .toEqual(['Памер', 'Зыходнае імя']);
  await expect(plainCard.getByText('Test Photographer')).toHaveCount(0);

  let activePublishRequests = 0;
  let maximumActivePublishRequests = 0;
  let publishRequests = 0;
  await page.route('**/admin/publish', async (route) => {
    activePublishRequests += 1;
    publishRequests += 1;
    maximumActivePublishRequests = Math.max(
      maximumActivePublishRequests,
      activePublishRequests,
    );
    try {
      await new Promise((resolve) => setTimeout(resolve, 200));
      const response = await route.fetch();
      await route.fulfill({ response });
    } finally {
      activePublishRequests -= 1;
    }
  });

  const publishForm = page.locator('[data-publish-form]');
  const publishButton = page.getByRole('button', {
    name: 'Запампаваць у Wikimedia Commons',
  });
  const publishSpinner = publishForm.locator('[data-publish-spinner]');
  const publishStatus = publishForm.locator('[data-publish-status]');
  await expect(publishForm.locator('input[data-publish-upload-id]')).toHaveCount(2);
  await expect(publishSpinner).toBeHidden();
  await publishButton.scrollIntoViewIfNeeded();
  await publishButton.click();
  await expect(publishButton).toBeDisabled();
  await expect(publishSpinner).toBeVisible();
  await expect(publishStatus).toHaveText(/^Запампоўваецца 1 з 2: /);
  await expect(publishStatus).toHaveText('Запампавана файлаў: 2.', { timeout: 120_000 });
  await expect(publishSpinner).toBeHidden();
  await expect(publishButton).toBeDisabled();
  await expect(publishForm.locator('input[data-publish-upload-id]')).toHaveCount(0);
  expect(publishRequests).toBe(2);
  expect(maximumActivePublishRequests).toBe(1);

  const results = page.locator('[data-publish-result-id].is-success');
  await expect(results).toHaveCount(2);
  const resultBounds = await results.evaluateAll((rows) => rows.map((row) => {
    const rectangle = row.getBoundingClientRect();
    return { bottom: rectangle.bottom, top: rectangle.top };
  }));
  expect(resultBounds[1].top).toBeGreaterThan(resultBounds[0].bottom);
  for (const filename of [renamedFilename, plainFilename]) {
    const row = results.filter({ hasText: filename });
    await expect(row).toHaveCount(1);
    await expect(row.getByRole('link', { name: filename, exact: true })).toHaveAttribute(
      'href',
      `https://commons.wikimedia.org/wiki/File:${
        encodeURIComponent(filename.replaceAll(' ', '_'))
      }`,
    );
    await expect(row.getByRole('link', { name: 'Прамы файл' })).toHaveAttribute(
      'href',
      `https://upload.wikimedia.org/mock/${encodeURIComponent(filename)}`,
    );
  }
  await expect(page.locator('.moderation-item')).toHaveCount(0);
  await expect(page.locator('[data-queue-count]')).toHaveText('0');
  await expect(page.locator('[data-queue-files]')).toHaveText('0 Б');
  await expect(page.locator('[data-queue-disk]')).toHaveText('0 МБ / 20000 МБ');
  await expect(page.getByRole('heading', { name: 'Чарга пустая' })).toBeVisible();
  await expect(page.locator('[data-pagination-slot] .pagination')).toHaveCount(0);
});

test('dark preference uses a pure black moderation background', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await loginAsModerator(page);
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(0, 0, 0)');
});
