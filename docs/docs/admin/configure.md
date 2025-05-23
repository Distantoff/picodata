# Конфигурирование

<style>
.md-typeset {
  .full,
  .absent,
  .partly,
  .cluster,
  .instance {
      padding: 0.1em 0.5em;
      border-radius: 0.5em;
      /* font-family: monospace; */
  }

  .full {
      background-color: #d9ead3;
  }

  .partly {
      background-color: #fff2cc;
  }

  .cluster {
      background-color: #fb93f1;
  }

  .instance {
      background-color: #93fbb1;
  }
}
</style>

В данном разделе описаны способы и особенности конфигурирования
инстансов Picodata.

## Типы параметров {: #paramater_types }

### Постоянные {: #persistent }

Постоянные параметры задаются один раз при запуске инстанса и затем не
могут быть изменены.

Постоянные параметры, относящиеся к `глобальным настройкам кластера`{.cluster},
устанавливаются в момент первоначальной сборки кластера и не могут быть
изменены без его повторной инициализации (bootstrap). Например:

- `PICODATA_CLUSTER_NAME`{.cluster} ([имя кластера](../reference/config.md#cluster_name))

Постоянные параметры, относящиеся к `отдельным инстансам`{.instance}, также
задаются один раз в начале при запуске инстанса. Менять их впоследствии нельзя.
Например:

- `PICODATA_INSTANCE_TIER`{.instance} ([имя тира, которому будет принадлежать инстанс](../reference/config.md#instance_tier))
- `PICODATA_INSTANCE_NAME`{.instance} ([имя инстанса](../reference/config.md#instance_name))

### Изменяемые {: #changeable }

Изменяемые параметры могут быть переопределены для действующих инстансов
в кластере. Такие параметры, в свою очередь, делятся на:

- `изменяемые без перезапуска инстанса`{.full}
- `изменяемые через перезапуск инстанса`{.partly}

Примеры изменяемых параметров:

- `SQL_VDBE_OPCODE_MAX / SQL_MOTION_ROW_MAX`{.full} ([неблокирующие запросы](../reference/sql/non_block.md#query_limitations))
- `PICODATA_CONFIG_FILE`{.partly} ([путь к файлу конфигурации](../reference/cli.md#run_config))
- `PICODATA_CONFIG_PARAMETERS`{.partly} ([список пар ключ-значение](../reference/cli.md#run_config_parameter))

Для изменения параметров, не требующих перезапуска инстанса, можно
использовать SQL-команду [ALTER SYSTEM](../reference/sql/alter_system.md).

Остальные параметры можно изменить, указав их новые значения при
повторном запуске инстанса (см. способы задания параметров ниже).

## Использование параметров командной строки {: #use_cli }

Исполняемый файл `picodata`, запускающий инстанс Picodata, поддерживает
разнообразные дополнительные параметры, с помощью которых можно явно
указать рабочую директорию инстанса, сетевой порт и т.д.

Данный способ удобен для:

- быстрого ознакомления с Picodata в UNIX shell на примере 1 или пары
  инстансов
- наглядного и оперативного контроля над всеми параметрами запуска
  инстанса Picodata

Пример использования аргументов командной строки:

```shell
picodata run --instance-dir ./data/i1 --iproto-listen 127.0.0.1:3301
```

Читайте далее:

- [Аргументы командной строки](../reference/cli.md)

## Использование переменных окружения {: #use_env }

У всех параметров запуска Picodata имеются аналогичные переменные
окружения, которые можно задавать в командной оболочке (например, Bash).

В примере выше был показан запуск инстанса с явным указанием его рабочей
директории и сетевого адреса. При использовании переменных окружения это
будет выглядеть так:

```shell
export PICODATA_INSTANCE_DIR="./data/i1"
export PICODATA_IPROTO_LISTEN="127.0.0.1:3301"
picodata run
```

Данный способ удобен для запуска нескольких инстансов (с разными
параметрами) на одном хосте. Наборы команд можно сохранить в виде
shell-скриптов, индивидуальных для каждого инстанса.

## Использование файла конфигурации {: #use_config }

Параметры запуска инстанса можно указать в файле конфигурации, и затем
передать путь к нему в качестве аргумента для `picodata run`.

Данный способ удобен для:

- запуска нескольких инстансов и работы с кластерными возможностями
  Picodata
- разделения глобальных настроек кластера и настроек отдельных инстансов
- удобного контроля над большим списком переопределяемых параметров
  запуска каждого инстанса
- использования разных наборов настроек для инстансов исходя из их
  назначения ([тиры], [домены отказа] и т.д.)

[тиры]: ../overview/glossary.md#tier
[домены отказа]: ../overview/glossary.md#failure_domain

Файл конфигурации представляет собой текстовый файл с YAML-разметкой.

Пример:

???+ example "my_cluster.yml"
    ```yaml
    cluster:
      name: my_cluster
    instance:
      instance_dir:
        ./mini1
      iproto_listen:
        127.0.0.1:3308
    ```
!!! note "Примечание"
    Файл конфигурации должен содержать имя кластера

Запуск инстанса с использованием файла конфигурации:

```shell
picodata run --config my_cluster.yml
```

Читайте далее:

- [Файл конфигурации](../reference/config.md)

## Приоритеты методов конфигурирования {: #priorities }

Поскольку разные методы установки параметров можно использовать
одновременно, важно учитывать их приоритет. В Picodata установлены
следующие приоритеты (от высшего к низшему):

1. Аргумент командной строки
1. Переменная окружения
1. Значение в файле конфигурации

Пример:

```shell
cat my_cluster.yaml
cluster:
  name: my_cluster
instance:
  instance_dir:
    ./mini1
  iproto_listen:
    127.0.0.1:3302

export PICODATA_IPROTO_LISTEN="127.0.0.1:3303"
picodata run --iproto-listen 127.0.0.1:3304 --config my_cluster.yaml
```

После запуска сетевой адрес инстанса будет `127.0.0.1:3304`.
