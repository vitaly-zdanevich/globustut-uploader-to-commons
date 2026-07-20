import { test, expect } from '@playwright/test';
import { execFileSync, spawnSync } from 'node:child_process';
import { mkdtempSync, readFileSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

test.describe.configure({ mode: 'serial' });

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
    gpsPhoto,
  ]);
});

test.afterAll(() => {
  if (fixtureDirectory) rmSync(fixtureDirectory, { recursive: true, force: true });
});

test('public upload and moderation workflow', async ({ page }, testInfo) => {
  const suffix = testInfo.project.name.startsWith('mobile') ? 'mobile' : 'desktop';
  const renamed = `Нясвіжскі замак ${suffix}`;

  await page.goto('/?category=Castles_in_Belarus');
  await expect(page.getByRole('heading', { name: 'Добавить фотографии' })).toBeVisible();
  expect(await page.evaluate(() => document.documentElement.scrollWidth <= window.innerWidth)).toBe(true);
  const publicCategory = page.getByRole('combobox', { name: 'Категория Wikimedia Commons' });
  await publicCategory.fill('Castl');
  await expect(page.getByRole('option', { name: 'Castles in Belarus' })).toBeVisible();
  await publicCategory.press('ArrowDown');
  await publicCategory.press('Enter');
  await expect(publicCategory).toHaveValue('Castles in Belarus');
  await page.locator('input[type=file]').setInputFiles({
    name: `castle-${suffix}.jpg`,
    mimeType: 'image/jpeg',
    buffer: readFileSync(gpsPhoto),
  });
  await expect(page.getByText(`castle-${suffix}.jpg`)).toBeVisible();
  await page.getByRole('checkbox').check();
  await page.getByRole('button', { name: 'Отправить на премодерацию' }).click();
  await expect(page.getByRole('heading', { name: 'Принято файлов: 1' })).toBeVisible();

  await page.goto('/admin');
  await expect(page).toHaveURL(/\/admin\/login$/);
  await page.getByLabel('Логин').fill('moderator');
  await page.getByLabel('Пароль').fill('correct horse battery staple');
  await page.getByRole('button', { name: 'Войти' }).click();
  await expect(page).toHaveURL(/\/admin$/);

  const card = page.locator('.moderation-item', { hasText: `castle-${suffix}` });
  await expect(card).toBeVisible();
  const preview = card.locator('.media-stage img');
  await expect(preview).toBeVisible();
  await expect.poll(() => preview.evaluate((image: HTMLImageElement) => image.naturalWidth))
    .toBeGreaterThan(0);
  await expect(card.getByRole('link', { name: /OpenStreetMap/ })).toHaveAttribute(
    'href',
    /mlat=53\.904500&mlon=27\.561500/,
  );
  await expect(card.getByText('2025-07-18')).toBeVisible();
  const nearby = card.locator('.nearby-category');
  await expect(nearby).toHaveCount(20);
  await expect(nearby.first()).toHaveText('Nearby category 25 · 0.1 км');
  await nearby.first().click();
  await expect(card.locator('input[name=category]')).toHaveValue('Nearby category 25');

  await card.locator('input[name=basename]').fill(renamed);
  await card.getByRole('button', { name: 'Сохранить имя' }).click();
  const currentCard = page.locator('.moderation-item').first();
  await expect(currentCard.locator('input[name=basename]')).toHaveValue(renamed);

  const categoryInput = currentCard.locator('input[name=category]');
  await categoryInput.fill('Castl');
  await expect(currentCard.getByRole('option', { name: 'Castles in Belarus' })).toBeVisible();
  await categoryInput.press('ArrowDown');
  await categoryInput.press('Enter');
  await expect(categoryInput).toHaveValue('Castles in Belarus');
  await categoryInput.fill('Castles of Belarus');
  await currentCard.getByRole('button', { name: 'Сохранить категорию' }).click();
  await expect(currentCard.locator('input[name=category]')).toHaveValue('Castles of Belarus');

  page.once('dialog', (dialog) => dialog.accept());
  await currentCard.getByRole('button', { name: 'Удалить файл' }).click();
  await expect(page.locator('.moderation-item')).toHaveCount(0);

  await page.goto('/');
  await page.locator('input[type=file]').setInputFiles({
    name: `plain-${suffix}.jpg`,
    mimeType: 'image/jpeg',
    buffer: readFileSync(plainPhoto),
  });
  await page.getByRole('combobox', { name: 'Категория Wikimedia Commons' }).fill('Belarus');
  await page.getByRole('checkbox').check();
  await page.getByRole('button', { name: 'Отправить на премодерацию' }).click();
  await page.goto('/admin');
  const noGpsCard = page.locator('.moderation-item').first();
  await expect(noGpsCard.getByText('Для поиска ближайших категорий нужны координаты EXIF.')).toBeVisible();
  await expect(noGpsCard.locator('[data-nearby-url]')).toHaveCount(0);
  page.once('dialog', (dialog) => dialog.accept());
  await noGpsCard.getByRole('button', { name: 'Удалить файл' }).click();
});

test('dark preference uses a pure black moderation background', async ({ page }) => {
  await page.emulateMedia({ colorScheme: 'dark' });
  await page.goto('/admin/login');
  await page.getByLabel('Логин').fill('moderator');
  await page.getByLabel('Пароль').fill('correct horse battery staple');
  await page.getByRole('button', { name: 'Войти' }).click();
  await expect(page.locator('body')).toHaveCSS('background-color', 'rgb(0, 0, 0)');
});
