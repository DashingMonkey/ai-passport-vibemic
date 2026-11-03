// tests/idf_stubs/lvgl.h —— LVGL API 语法桩(只声明出货代码用到的部分)。
// 真实实现在 managed_components/lvgl;这里只为 `-fsyntax-only` 提供类型,
// 行为一律不模拟(任何断言都不应依赖本桩)。
#pragma once

#include <stdint.h>

typedef struct { uint8_t blue, green, red; } lv_color_t;
typedef struct { int unused; } lv_obj_t;
typedef struct { int unused; } lv_font_t;

extern const lv_font_t lv_font_montserrat_14;
extern const lv_font_t lv_font_montserrat_20;

#define LV_ALIGN_TOP_LEFT 0
#define LV_ALIGN_TOP_MID 1
#define LV_ALIGN_TOP_RIGHT 2
#define LV_ALIGN_CENTER 3
#define LV_ALIGN_BOTTOM_MID 4
#define LV_SYMBOL_BULLET "\xE2\x80\xA2"
#define LV_SYMBOL_TRASH "\xEF\x8B\xAD"

lv_color_t lv_color_hex(uint32_t value);
lv_obj_t *lv_obj_create(lv_obj_t *parent);
lv_obj_t *lv_label_create(lv_obj_t *parent);
void lv_obj_set_style_bg_color(lv_obj_t *object, lv_color_t color, uint32_t selector);
void lv_obj_set_style_text_color(lv_obj_t *object, lv_color_t color, uint32_t selector);
void lv_obj_set_style_text_font(lv_obj_t *object, const lv_font_t *font, uint32_t selector);
void lv_obj_align(lv_obj_t *object, int align, int x_ofs, int y_ofs);
void lv_label_set_text(lv_obj_t *object, const char *text);
void lv_label_set_text_fmt(lv_obj_t *object, const char *format, ...);
void lv_screen_load(lv_obj_t *screen);
