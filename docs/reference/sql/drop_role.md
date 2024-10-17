# DROP ROLE

[DCL](dcl.md)-команда `DROP ROLE` используется для удаления существующей
[роли](../../tutorial/access_control.md#roles).

## Синтаксис {: #syntax }

![DROP ROLE](../../images/ebnf/drop_role.svg)

## Параметры {: #params }

* **ROLE** — имя роли. Соответствует правилам имен для всех
  [объектов](object.md) в кластере
* **IF EXISTS** — позволяет избежать ошибки в случае, если такой
  роли в кластере нет

## Примеры {: #examples }

```sql
DROP ROLE toy OPTION (TIMEOUT = 3.0);
```
