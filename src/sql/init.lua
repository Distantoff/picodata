require('sbroad.core-router')
local helper = require('sbroad.helper')
local utils = require('internal.utils')
local check_param_table = utils.check_param_table

local function sql(...)
    local n_args = select("#", ...)
    if n_args == 0 or n_args > 3 then
	      return nil, "Usage: sql(query[, params, options])"
    end
    local query, params, options = ...
    if type(query) ~= "string" then
	      return nil, "SQL query must be a string"
    end
    if params ~= nil and type(params) ~= "table" then
	      return nil, "SQL params must be a table"
    end
    if options ~= nil and type(options) ~= "table" then
	      return nil, "SQL options must be a table"
    end
    check_param_table(options, {
        query_id = 'string',
        traceable = 'boolean',
    })

    local query_id = box.NULL
    if options ~= nil then
        query_id = options.query_id or box.NULL
    end

    local traceable = box.NULL
    if options ~= nil then
	      traceable = options.traceable or box.NULL
    end

    if params == nil then
        params = {}
    end

    local ok, res = pcall(
        function()
            return box.func[".proc_sql_dispatch"]:call({
                query, params, query_id, traceable
	    })
        end
    )

    if ok == false then
        if res.code == box.error.SQL_SYNTAX_NEAR_TOKEN then
            res = tostring(res)
        end
        return nil, res
    end

    return helper.format_result(res[1])
end

return {
    sql	= sql,
}
