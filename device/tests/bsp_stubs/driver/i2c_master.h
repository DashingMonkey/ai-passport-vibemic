// tests/bsp_stubs/driver/i2c_master.h —— 语法检查用的 I2C master 句柄类型。
// 真实的 bsp_i2c.h 需要它(真定义在 ESP-IDF driver/i2c_master.h)。
#pragma once

typedef struct i2c_master_bus_t *i2c_master_bus_handle_t;
typedef struct i2c_master_dev_t *i2c_master_dev_handle_t;
