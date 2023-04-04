import tarantool from "k6/x/tarantool";
import { updateSuccessRate } from '../metrics.js';

const clients = [
    tarantool.connect(["localhost:3301"], {"user": "admin", pass: "app-cluster-cookie"}),
]

const pattern = `SELECT "a0"+"a1"+"a2", "a1", "a1"+"a2", "a3", "a4", "a5" + "a1", "a1"+"a6", "a7"*"a9", "a8", "a9" 
                FROM "t"
                GROUP BY "a1", "a3", "a4", "a0"+"a1"+"a2", "a5" + "a1", "a1"+"a2",  "a8", "a9", "a1"+"a6", "a7"*"a9"
`

export default () => {
    var resp = tarantool.call(clients[0], "sbroad.execute", [pattern, []]);
    updateSuccessRate(resp);
}